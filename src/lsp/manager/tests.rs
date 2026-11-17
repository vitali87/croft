//! The language-server manager's own tests, moved out of manager.rs (#2062).

use super::*;
use lsp_types::{
    LanguageString, Location, LocationLink, MarkupContent, SemanticTokensLegend,
    SemanticTokensOptions,
};
use std::time::{Duration, Instant};

/// #1574: the server follows `files_associations`: a `.tpl` associated
/// with HTML gets the HTML server, and a file associated with a
/// language that has no server gets none, not its extension's.
#[test]
fn an_associated_file_gets_its_associations_server() {
    let tmp = tempfile::tempdir().unwrap();
    let map = [
        ("*.tpl", "html"),
        ("*.rs", "plaintext"),
        ("*.py", "klingon"),
    ]
    .iter()
    .map(|(g, id)| (g.to_string(), id.to_string()))
    .collect();
    crate::highlight::set_file_associations(tmp.path(), &map);
    assert_eq!(
        path_to_language(&tmp.path().join("card.tpl")),
        crate::lsp::languages::resolve("html")
    );
    assert!(crate::lsp::languages::resolve("html").is_some());
    assert_eq!(path_to_language(&tmp.path().join("main.rs")), None);
    assert_eq!(
        path_to_language(&tmp.path().join("lib.py")),
        crate::lsp::languages::resolve("python"),
        "an unknown association id keeps its extension's server"
    );
    assert!(crate::lsp::languages::resolve("python").is_some());
    assert_eq!(
        path_to_language(&tmp.path().join("main.go")),
        crate::lsp::languages::resolve("go"),
        "an unassociated file keeps its extension's server"
    );
}

/// #1532: a `.scss` or `.less` file resolves to its own language, which
/// the CSS server is started for; `.sass` resolves to none.
#[test]
fn scss_and_less_files_resolve_to_css_server_languages() {
    assert_eq!(
        path_to_language(Path::new("src/app.scss")).map(|l| l.lsp_id()),
        Some("scss")
    );
    assert_eq!(
        path_to_language(Path::new("src/theme.less")).map(|l| l.lsp_id()),
        Some("less")
    );
    assert_eq!(path_to_language(Path::new("old.sass")), None);
    assert_eq!(
        path_to_language(Path::new("plain.css")),
        Some(Language::CSS)
    );
}

impl LspManager {
    /// Test seam for App-level completion tests: swap the completion
    /// reply channel for one the test holds, so a reply lands in
    /// `drain_completion` at a chosen moment, as a slow server's does.
    /// The worker's own sender then goes nowhere, which a test-built App
    /// (no real servers) never uses anyway.
    pub(crate) fn completion_reply_sender_for_test(
        &mut self,
    ) -> std_mpsc::Sender<CompletionResult> {
        let (tx, rx) = std_mpsc::channel();
        self.completion_rx = rx;
        tx
    }
}

/// #1527: an item's documentation and its `labelDetails` signature
/// survive the conversion; they were dropped, so nothing could show them.
#[test]
fn a_completion_item_keeps_its_documentation_and_label_details() {
    let item: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
        "label": "trim",
        "detail": "fn(&self) -> &str",
        "documentation": { "kind": "markdown", "value": "Returns a string slice." }
    }))
    .unwrap();
    let ours = into_item(item);
    assert_eq!(
        ours.documentation.as_deref(),
        Some("Returns a string slice.")
    );
    assert_eq!(ours.detail.as_deref(), Some("fn(&self) -> &str"));

    let item: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
        "label": "dumps",
        "labelDetails": { "detail": "(obj, ...)", "description": "def dumps(obj) -> str" },
        "documentation": "Serialize obj to a JSON formatted str."
    }))
    .unwrap();
    let ours = into_item(item);
    assert_eq!(
        ours.detail.as_deref(),
        Some("(obj, ...) def dumps(obj) -> str")
    );
    assert_eq!(
        ours.documentation.as_deref(),
        Some("Serialize obj to a JSON formatted str.")
    );
}

/// #1527: `labelDetails.detail` is the signature and `description` a
/// separate qualifier; having both must not drop the signature.
#[test]
fn label_details_keep_the_signature_beside_the_description() {
    let item: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
        "label": "dumps",
        "labelDetails": { "detail": "(obj, ...)", "description": "json" }
    }))
    .unwrap();
    assert_eq!(into_item(item).detail.as_deref(), Some("(obj, ...) json"));

    let item: lsp_types::CompletionItem = serde_json::from_value(serde_json::json!({
        "label": "dumps",
        "labelDetails": { "detail": "(obj, ...)", "description": "  " }
    }))
    .unwrap();
    assert_eq!(into_item(item).detail.as_deref(), Some("(obj, ...)"));
}

/// #1527 negative: an item with neither keeps neither.
#[test]
fn a_bare_completion_item_has_no_documentation() {
    let item: lsp_types::CompletionItem =
        serde_json::from_value(serde_json::json!({ "label": "x" })).unwrap();
    let ours = into_item(item);
    assert_eq!(ours.documentation, None);
    assert_eq!(ours.detail, None);
}

/// #928: an item from a server that resolves lazily is kept whole,
/// `data` and all, to send back on accept, and croft lets servers defer
/// imports to that resolve. An item that already carries its imports,
/// or one from a server that does not resolve, needs no round trip.
#[test]
fn a_lazily_resolved_completion_item_is_kept_for_resolve() {
    let lazy = serde_json::json!({ "label": "total", "data": { "entryNames": ["total"] } });
    let item = serde_json::from_value(lazy.clone()).unwrap();
    let kept = listed_item(item, true).resolve.expect("kept for resolve");
    assert_eq!(serde_json::to_value(&*kept).unwrap(), lazy);
    let item = serde_json::from_value(lazy).unwrap();
    assert!(into_item(item).resolve.is_none());
    let at = serde_json::json!({ "line": 1, "character": 0 });
    let edit = serde_json::json!({ "range": { "start": at, "end": at }, "newText": "x" });
    let eager = serde_json::json!({ "label": "Session", "additionalTextEdits": [edit] });
    let eager = serde_json::from_value(eager).unwrap();
    assert!(listed_item(eager, true).resolve.is_none());
    let resolvable = build_client_capabilities()
        .text_document
        .and_then(|td| td.completion)
        .and_then(|c| c.completion_item)
        .and_then(|i| i.resolve_support)
        .expect("completionItem.resolveSupport is declared")
        .properties;
    assert_eq!(resolvable, ["additionalTextEdits"]);
}

#[test]
fn normalise_inlay_hint_flattens_parts_and_folds_padding() {
    // vtsls sends parts labels; rust-analyzer sends plain strings with
    // padding flags. Both must land as one splice-ready string.
    let parts = lsp_types::InlayHint {
        position: Position {
            line: 3,
            character: 14,
        },
        label: lsp_types::InlayHintLabel::LabelParts(vec![
            lsp_types::InlayHintLabelPart {
                value: ": ".into(),
                ..Default::default()
            },
            lsp_types::InlayHintLabelPart {
                value: "Vec<String>".into(),
                ..Default::default()
            },
        ]),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: None,
        data: None,
    };
    let item = normalise_inlay_hint(parts);
    assert_eq!(item.line, 3);
    assert_eq!(item.character, 14);
    assert_eq!(item.label, ": Vec<String>");

    let padded = lsp_types::InlayHint {
        position: Position {
            line: 0,
            character: 9,
        },
        label: lsp_types::InlayHintLabel::String("param:".into()),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: Some(true),
        data: None,
    };
    assert_eq!(normalise_inlay_hint(padded).label, " param: ");

    let multiline = lsp_types::InlayHint {
        position: Position {
            line: 0,
            character: 0,
        },
        label: lsp_types::InlayHintLabel::String("a\nb".into()),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: None,
        data: None,
    };
    assert_eq!(
        normalise_inlay_hint(multiline).label,
        "a b",
        "a newline would corrupt the spliced row"
    );
}

#[test]
fn workspace_symbol_items_maps_the_flat_response() {
    use lsp_types::{SymbolInformation, SymbolKind, Url, WorkspaceSymbolResponse};
    #[allow(deprecated)]
    let resp = WorkspaceSymbolResponse::Flat(vec![SymbolInformation {
        name: "main".into(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        location: Location {
            uri: Url::from_file_path("/w/src/main.rs").unwrap(),
            range: lsp_types::Range {
                start: Position {
                    line: 4,
                    character: 3,
                },
                end: Position {
                    line: 4,
                    character: 7,
                },
            },
        },
        container_name: Some("app".into()),
    }]);
    let items = workspace_symbol_items(resp);
    assert_eq!(
        items,
        vec![WorkspaceSymbolItem {
            name: "main".into(),
            kind: OutlineKind::Function,
            path: PathBuf::from("/w/src/main.rs"),
            line: 4,
            character: 3,
            container: Some("app".into()),
        }]
    );
}

#[test]
fn workspace_symbol_items_maps_the_nested_response_and_bare_uris() {
    use lsp_types::{SymbolKind, Url, WorkspaceLocation, WorkspaceSymbol, WorkspaceSymbolResponse};
    let resp = WorkspaceSymbolResponse::Nested(vec![
        WorkspaceSymbol {
            name: "Config".into(),
            kind: SymbolKind::STRUCT,
            tags: None,
            container_name: None,
            location: OneOf::Left(Location {
                uri: Url::from_file_path("/w/src/config.rs").unwrap(),
                range: lsp_types::Range {
                    start: Position {
                        line: 9,
                        character: 0,
                    },
                    end: Position {
                        line: 9,
                        character: 6,
                    },
                },
            }),
            data: None,
        },
        WorkspaceSymbol {
            name: "helper".into(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            container_name: None,
            location: OneOf::Right(WorkspaceLocation {
                uri: Url::from_file_path("/w/src/lib.rs").unwrap(),
            }),
            data: None,
        },
    ]);
    let items = workspace_symbol_items(resp);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind, OutlineKind::Struct);
    assert_eq!(items[0].line, 9);
    assert_eq!(
        items[1].path,
        PathBuf::from("/w/src/lib.rs"),
        "a range-less nested location still yields a jump target"
    );
    assert_eq!(items[1].line, 0, "bare-URI locations land on line 0");
}

#[test]
fn normalise_signature_help_resolves_active_param_range() {
    use lsp_types::{ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation};
    // Two params; help-level active_parameter = 1 (the second, "b: i32").
    let help = SignatureHelp {
        signatures: vec![SignatureInformation {
            label: "foo(a: i32, b: i32)".to_string(),
            documentation: None,
            parameters: Some(vec![
                ParameterInformation {
                    label: ParameterLabel::LabelOffsets([4, 10]),
                    documentation: None,
                },
                ParameterInformation {
                    label: ParameterLabel::LabelOffsets([12, 18]),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(1),
    };
    let (sigs, active) = normalise_signature_help(help);
    assert_eq!(active, 0);
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].active_param, Some((12, 18)));
}

#[test]
fn normalise_signature_help_resolves_simple_label_to_range() {
    use lsp_types::{ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation};
    let help = SignatureHelp {
        signatures: vec![SignatureInformation {
            label: "foo(a, b)".to_string(),
            documentation: None,
            parameters: Some(vec![ParameterInformation {
                label: ParameterLabel::Simple("a".to_string()),
                documentation: None,
            }]),
            active_parameter: None,
        }],
        active_signature: None,
        active_parameter: Some(0),
    };
    let (sigs, _) = normalise_signature_help(help);
    // "a" sits at char index 4 in "foo(a, b)".
    assert_eq!(sigs[0].active_param, Some((4, 5)));
}

#[test]
fn client_capabilities_advertise_work_done_progress() {
    // Servers only emit `$/progress` (rust-analyzer's "Indexing…") when the
    // client declares `window.workDoneProgress`. Without this the status bar
    // could never show indexing state.
    let caps = build_client_capabilities();
    let window = caps.window.expect("window capabilities must be set");
    assert_eq!(window.work_done_progress, Some(true));
}

/// gopls only sends "Browse documentation" as a link to open, rather
/// than as a line nobody sees, to a client that says it can show
/// documents; servers only ask questions with actions of one that says
/// it can answer them (#1599).
#[test]
fn client_capabilities_offer_show_document_and_message_actions() {
    let window = build_client_capabilities().window.expect("window");
    assert_eq!(
        window.show_document.map(|d| d.support),
        Some(true),
        "window.showDocument"
    );
    assert!(
        window
            .show_message
            .and_then(|m| m.message_action_item)
            .is_some(),
        "window.showMessage.messageActionItem"
    );
}

#[test]
fn only_errors_warnings_and_links_interrupt_the_user() {
    use lsp_types::MessageType as MT;
    assert!(message_is_shown(MT::ERROR, "Failed to load workspaces."));
    assert!(message_is_shown(MT::WARNING, "cargo check failed"));
    assert!(message_is_shown(
        MT::INFO,
        "Gopls: open your browser to https://go.dev/gopls"
    ));
    assert!(!message_is_shown(MT::INFO, "Indexing done"));
    assert!(!message_is_shown(MT::LOG, "https://example.com"));
}

/// Without `linkSupport` a server turns each `LocationLink` into a
/// `Location` spanning the whole declaration, so F12 lands on its first
/// character, lines above a name in a multi-line declaration (#1284).
#[test]
fn client_capabilities_ask_for_location_links_on_every_goto() {
    let caps = build_client_capabilities();
    let td = caps.text_document.expect("text document capabilities");
    for (name, goto) in [
        ("definition", td.definition),
        ("declaration", td.declaration),
        ("typeDefinition", td.type_definition),
        ("implementation", td.implementation),
    ] {
        let goto = goto.unwrap_or_else(|| panic!("{name} capability must be declared"));
        assert_eq!(goto.link_support, Some(true), "{name}.linkSupport");
        assert_eq!(goto.dynamic_registration, Some(false), "{name}");
    }
}

/// Negative: a server that still answers with a plain `Location`
/// jumps to its start, and a link whose declaration starts lines above
/// its name lands on the name, not the declaration.
#[test]
fn a_plain_location_still_jumps_to_its_start_and_a_link_to_its_name() {
    let uri = Url::from_file_path("/tmp/config.ts").unwrap();
    let plain = GotoDefinitionResponse::Array(vec![Location {
        uri: uri.clone(),
        range: lsp_types::Range::new(Position::new(7, 0), Position::new(11, 10)),
    }]);
    assert_eq!(
        def_location(&plain),
        Some((PathBuf::from("/tmp/config.ts"), 7, 0))
    );
    let link = GotoDefinitionResponse::Link(vec![LocationLink {
        origin_selection_range: None,
        target_uri: uri,
        target_range: lsp_types::Range::new(Position::new(7, 0), Position::new(11, 10)),
        target_selection_range: lsp_types::Range::new(Position::new(10, 2), Position::new(10, 7)),
    }]);
    assert_eq!(
        def_location(&link),
        Some((PathBuf::from("/tmp/config.ts"), 10, 2))
    );
    assert_eq!(
        def_locations(&link),
        vec![(PathBuf::from("/tmp/config.ts"), 10, 2)]
    );
}

#[test]
fn client_capabilities_advertise_workspace_folders() {
    // Per LSP 3.6+, a server may IGNORE the `workspace_folders` array in
    // initialize unless the client declares `workspace.workspaceFolders`;
    // without it, folder-aware servers (rust-analyzer, gopls, ruff, ty)
    // legally fall back to single-root behavior.
    let caps = build_client_capabilities();
    let ws = caps.workspace.expect("workspace capabilities must be set");
    assert_eq!(
        ws.workspace_folders,
        Some(true),
        "the workspaceFolders capability gates every folder-aware server feature"
    );
}

/// A stub language server that completes the LSP handshake, advertises
/// `workspaceDiagnostics`, and then NEVER answers a `workspace/diagnostic`
/// request — exactly what `ty` 0.0.73 does, and the shape that caused the
/// #533 regression. Everything else it answers normally.
///
/// Written to a temp dir and run under the test's own Python, so the test
/// needs no server on PATH.
/// Returns `(script, receipt)`: the stub APPENDS one line to `receipt`
/// for every `workspace/diagnostic` request that reaches it, so a test can
/// assert both that a pull arrived and how many did.
fn mute_pull_server_script(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let path = dir.join("mute_pull_server.py");
    let receipt = dir.join("pull-received");
    std::fs::write(
        &path,
        r#"
import json, sys

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def write(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        write({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {
            "diagnosticProvider": {
                "interFileDependencies": True,
                "workspaceDiagnostics": True,
            },
            "definitionProvider": True,
        }}})
    elif method == "workspace/diagnostic":
        # Record receipt (one line per request), so a test waits for the
        # request to ARRIVE rather than guessing with a sleep, and can count
        # how many arrived. Then never answer -- the bug.
        with open(sys.argv[1], "a") as f:
            f.write("pull\n")
    elif method == "shutdown":
        write({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif "id" in msg:
        write({"jsonrpc": "2.0", "id": msg["id"], "result": None})
"#,
    )
    .expect("write stub server");
    (path, receipt)
}

/// A `ManagedClient` wrapping `client`, advertising workspace diagnostics
/// and nothing else — the shape `workspace_pull_targets` filters over.
fn managed_client_for_pull_test(client: LspClient) -> ManagedClient {
    ManagedClient {
        name: String::from("mute-pull"),
        client: Arc::new(TokioMutex::new(client)),
        supports_workspace_diagnostics: true,
        diagnostic_identifier: None,
        diagnostic_result_ids: Arc::new(TokioMutex::new(DiagnosticResultIds::default())),
        diagnostic_pull_stalled: Arc::new(AtomicBool::new(false)),
        save_notify: SaveNotify::Never,
        supports_document_diagnostics: false,
        document_pull_stalled: Arc::new(AtomicBool::new(false)),
        prepare_rename_stalled: Arc::new(AtomicBool::new(false)),
        document_pull_generations: Arc::new(StdMutex::new(HashMap::new())),
        supports_completion: false,
        completion_triggers: String::new(),
        supports_signature_help: false,
        supports_hover: false,
        supports_definition: false,
        supports_document_symbol: false,
        supports_declaration: false,
        supports_type_definition: false,
        supports_implementation: false,
        supports_references: false,
        supports_document_highlight: false,
        supports_linked_editing: false,
        supports_inline_completion: false,
        supports_selection_range: false,
        supports_call_hierarchy: false,
        supports_type_hierarchy: false,
        supports_workspace_symbols: false,
        supports_rename: false,
        supports_prepare_rename: false,
        supports_formatting: false,
        on_type_triggers: None,
        supports_range_formatting: false,
        supports_code_action: false,
        semantic_legend: None,
        semantic_supports_range: false,
        supports_inlay_hints: false,
        supports_document_link: false,
        supports_code_lens: false,
        code_lens_resolve: false,
        server_commands: Arc::new(Vec::new()),
        supports_folding_range: false,
        supports_document_color: false,
        will_rename: None,
        did_rename: None,
    }
}

/// A pull already QUEUED when the server is retired must issue nothing
/// (#533).
///
/// `workspace_pull_targets` reads the retirement flag when it builds the
/// target list, so a second pull spawned before the first expires carries
/// a stale snapshot. It then waits the whole ceiling on the result-id lock
/// the first pull holds, and would wake and send exactly the un-reapable
/// request the retirement exists to prevent. Both targets are built up
/// front here, which is what makes the snapshot stale.
#[test]
fn a_pull_queued_before_retirement_issues_no_request() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (script, receipt) = mute_pull_server_script(&root);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    runtime.block_on(async {
        let config = ServerConfig {
            name: "mute-pull",
            command: python.to_string_lossy().into_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                receipt.to_string_lossy().into_owned(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        };
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let client = LspClient::spawn(
            &config,
            &root,
            build_client_capabilities(),
            &[],
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
        )
        .await
        .expect("the stub server handshakes");
        let managed = managed_client_for_pull_test(client);

        // Both target lists are built BEFORE either pull runs, so the
        // second carries a snapshot taken while the server was healthy.
        let first = workspace_pull_targets(std::iter::once(&managed));
        let second = workspace_pull_targets(std::iter::once(&managed));
        assert_eq!(second.len(), 1, "the second pull must start un-retired");

        let (tx, _rx) = std_mpsc::channel();
        spawn_workspace_pull_within(first, tx.clone(), std::time::Duration::from_millis(200));
        spawn_workspace_pull_within(second, tx, std::time::Duration::from_millis(200));

        let retired = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !managed.diagnostic_pull_stalled.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(retired.is_ok(), "the first pull must retire the server");

        // Give the queued second pull time to wake and (wrongly) send.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let pulls = std::fs::read_to_string(&receipt).unwrap_or_default();
        assert_eq!(
            pulls.lines().count(),
            1,
            "a pull queued before retirement must issue no request: each \
                 one leaves an entry in async-lsp's outgoing map that only a \
                 response can drain, and this server never answers"
        );
    });
}

/// A stub language server that advertises `diagnosticProvider` without
/// workspace pulls, as rumdl and the VS Code JSON server do (#866). Each
/// `textDocument/diagnostic` it receives appends a line to `receipt`;
/// with `answer` it replies with one error, "stub problem", and without
/// it never replies.
fn document_pull_server_script(dir: &Path, answer: bool) -> (PathBuf, PathBuf) {
    let path = dir.join(format!("doc_pull_server_{answer}.py"));
    let receipt = dir.join(format!("doc-pulls-{answer}"));
    let reply = if answer {
        r#"write({"jsonrpc": "2.0", "id": msg["id"], "result": {"kind": "full", "items": [
                {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                 "severity": 1, "message": "stub problem"}]}})"#
    } else {
        "pass"
    };
    std::fs::write(
        &path,
        format!(
            r#"
import json, sys

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def write(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        write({{"jsonrpc": "2.0", "id": msg["id"], "result": {{"capabilities": {{
            "diagnosticProvider": {{"interFileDependencies": False, "workspaceDiagnostics": False}}
        }}}}}})
    elif method == "textDocument/diagnostic":
        with open(sys.argv[1], "a") as f:
            f.write("pull\n")
        {reply}
    elif "id" in msg:
        write({{"jsonrpc": "2.0", "id": msg["id"], "result": None}})
"#
        ),
    )
    .expect("write stub server");
    (path, receipt)
}

/// Spawn the document-pull stub and wrap it as a client that answers
/// `textDocument/diagnostic`, the shape `document_pull_targets` keeps.
async fn document_pull_client(
    python: &Path,
    root: &Path,
    answer: bool,
) -> (ManagedClient, PathBuf) {
    let (script, receipt) = document_pull_server_script(root, answer);
    let config = ServerConfig {
        name: "doc-pull",
        command: python.to_string_lossy().into_owned(),
        args: vec![
            script.to_string_lossy().into_owned(),
            receipt.to_string_lossy().into_owned(),
        ],
        language: Language::MARKDOWN,
        initialization_options: None,
        provision: None,
    };
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let client = LspClient::spawn(
        &config,
        root,
        build_client_capabilities(),
        &[],
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        diag_tx,
        prog_tx,
    )
    .await
    .expect("the stub server handshakes");
    let mut managed = managed_client_for_pull_test(client);
    managed.name = String::from("doc-pull");
    managed.supports_workspace_diagnostics = false;
    managed.supports_document_diagnostics = true;
    (managed, receipt)
}

fn stub_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime")
}

/// #866: a server that only reports when asked has its answer forwarded
/// under its own name, on the channel a push lands on, so the app stores
/// it like any published set.
#[test]
fn a_document_pull_forwards_the_report_under_the_servers_name() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("a.md");
    let uri = Url::from_file_path(&file).expect("a file uri");
    let support: CapabilitySupport = Arc::default();
    stub_runtime().block_on(async {
        let (managed, _receipt) = document_pull_client(&python, &root, true).await;
        let pulls = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        assert_eq!(pulls.len(), 1, "a diagnosticProvider server is pulled");
        let (tx, rx) = std_mpsc::channel();
        spawn_document_pulls_within(
            pulls,
            tx,
            std::time::Duration::ZERO,
            std::time::Duration::from_secs(20),
        );
        let update = tokio::task::spawn_blocking(move || {
            rx.recv_timeout(crate::test_budget::spawn_budget(
                std::time::Duration::from_secs(5),
            ))
        })
        .await
        .expect("the waiter")
        .expect("the pulled report arrives");
        assert_eq!(update.path, file);
        assert_eq!(update.server, "doc-pull");
        let messages: Vec<&str> = update
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(messages, ["stub problem"]);
    });
}

/// #866: while typing, only the newest pull for a document goes out. One
/// an edit overtook is dropped before it is sent, so a slow answer about
/// older text can never land over a newer one.
#[test]
fn a_document_pull_an_edit_overtook_is_never_sent() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("a.md");
    let uri = Url::from_file_path(&file).expect("a file uri");
    let support: CapabilitySupport = Arc::default();
    stub_runtime().block_on(async {
        let (managed, receipt) = document_pull_client(&python, &root, true).await;
        let older = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        let newer = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        let (tx, rx) = std_mpsc::channel();
        let ceiling = std::time::Duration::from_secs(20);
        spawn_document_pulls_within(older, tx.clone(), std::time::Duration::ZERO, ceiling);
        spawn_document_pulls_within(newer, tx, std::time::Duration::ZERO, ceiling);
        let first = tokio::task::spawn_blocking(move || {
            let budget = crate::test_budget::spawn_budget(std::time::Duration::from_secs(5));
            let first = rx.recv_timeout(budget);
            // Room for a wrongly sent second answer to arrive too.
            let second = rx.recv_timeout(std::time::Duration::from_millis(500));
            (first, second)
        })
        .await
        .expect("the waiter");
        assert!(first.0.is_ok(), "the newest pull is answered");
        assert!(first.1.is_err(), "and only it");
        let pulls = std::fs::read_to_string(&receipt).unwrap_or_default();
        assert_eq!(
            pulls.lines().count(),
            1,
            "the overtaken pull never went out"
        );
    });
}

/// #866: a pull still out when its document closes is not the latest
/// pull once the document reopens, so its late answer, about the text
/// before the close, can never land on the reopened buffer.
#[test]
fn a_pull_from_before_a_close_is_stale_after_the_reopen() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("a.md");
    let uri = Url::from_file_path(&file).expect("a file uri");
    let support: CapabilitySupport = Arc::default();
    stub_runtime().block_on(async {
        let (managed, _receipt) = document_pull_client(&python, &root, true).await;
        let before = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        // What `close_doc` does to the document's generation.
        managed
            .document_pull_generations
            .lock()
            .unwrap()
            .remove(&file);
        let after = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        assert!(after[0].is_current(), "the reopened document's pull");
        assert!(
            !before[0].is_current(),
            "the pull from before the close is stale"
        );
    });
}

/// #866: a server that lets a document pull go unanswered is retired
/// from document pulls, as one that stalls a workspace pull is (#533):
/// every unanswered request leaks an entry async-lsp can never drain. It
/// then reports on nothing: its pulled sets are withdrawn and it leaves
/// the file's reporters, so the built-in Markdown lint returns where it
/// stood down for rumdl (#851).
#[test]
fn an_unanswered_document_pull_retires_the_server() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("a.md");
    let uri = Url::from_file_path(&file).expect("a file uri");
    let support: CapabilitySupport = Arc::default();
    stub_runtime().block_on(async {
        let (managed, _receipt) = document_pull_client(&python, &root, false).await;
        // Opened with another server beside it (#851).
        support.lock().unwrap().document_servers.insert(
            file.clone(),
            vec![String::from("doc-pull"), String::from("other")],
        );
        support.lock().unwrap().document_clients.insert(
            file.clone(),
            vec![String::from("doc-pull"), String::from("other")],
        );
        let pulls = document_pull_targets(std::iter::once(&managed), &file, &uri, &support);
        let (tx, rx) = std_mpsc::channel();
        spawn_document_pulls_within(
            pulls,
            tx,
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(200),
        );
        let retired = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !managed.document_pull_stalled.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(retired.is_ok(), "the unanswered pull retires the server");
        assert!(
            document_pull_targets(std::iter::once(&managed), &file, &uri, &support).is_empty(),
            "and it is not asked again"
        );
        let withdrawn = tokio::task::spawn_blocking(move || {
            rx.recv_timeout(crate::test_budget::spawn_budget(
                std::time::Duration::from_secs(5),
            ))
        })
        .await
        .expect("the waiter")
        .expect("its pulled set is withdrawn");
        assert_eq!(
            (withdrawn.path, withdrawn.server.as_str()),
            (file.clone(), "doc-pull")
        );
        assert!(withdrawn.diagnostics.is_empty());
        assert_eq!(
            support.lock().unwrap().document_servers[&file],
            ["other"],
            "it no longer counts as reporting on the file, and only it left"
        );
        assert_eq!(
            support.lock().unwrap().document_clients[&file],
            ["doc-pull", "other"],
            "but its client still runs, so it still serves the file (#1626)"
        );
    });
}

/// `workspace_pull_targets` must DROP a retired server, so the retirement
/// actually stops the next pull rather than only recording a flag (#533).
///
/// This is the end of the leak fix: `workspace/diagnostic/refresh` is
/// server-driven and unrated, so without the filter a stalling server is
/// re-asked on every refresh and each abandoned request leaves another
/// entry in async-lsp's `outgoing` map that nothing can drain.
#[test]
fn a_retired_server_is_dropped_from_the_next_pull() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (script, receipt) = mute_pull_server_script(&root);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    runtime.block_on(async {
        let config = ServerConfig {
            name: "mute-pull",
            command: python.to_string_lossy().into_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                receipt.to_string_lossy().into_owned(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        };
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let client = LspClient::spawn(
            &config,
            &root,
            build_client_capabilities(),
            &[],
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
        )
        .await
        .expect("the stub server handshakes");

        // A `ManagedClient` as `ensure_clients` builds one, which is what
        // `workspace_pull_targets` actually filters over in production.
        let managed = managed_client_for_pull_test(client);
        assert_eq!(
            workspace_pull_targets(std::iter::once(&managed)).len(),
            1,
            "a healthy server advertising the capability must be pulled"
        );

        let (tx, _rx) = std_mpsc::channel();
        spawn_workspace_pull_within(
            workspace_pull_targets(std::iter::once(&managed)),
            tx.clone(),
            std::time::Duration::from_millis(200),
        );

        // Wait for the retirement the timeout records.
        let retired = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !managed.diagnostic_pull_stalled.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(retired.is_ok(), "the expired pull must retire the server");

        let after_first = std::fs::read_to_string(&receipt).unwrap_or_default();
        assert_eq!(
            after_first.lines().count(),
            1,
            "the first pull must actually reach the server"
        );

        // The refresh path: a second pull for the same clients. The
        // retired server must be filtered out, so the stub sees nothing
        // more and no second un-reapable request is issued.
        assert!(
            workspace_pull_targets(std::iter::once(&managed)).is_empty(),
            "a retired server must be dropped from later pulls, or every \
                 refresh leaks another request that can never be reaped"
        );
        spawn_workspace_pull_within(
            workspace_pull_targets(std::iter::once(&managed)),
            tx,
            std::time::Duration::from_millis(200),
        );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let after_second = std::fs::read_to_string(&receipt).unwrap_or_default();
        assert_eq!(
            after_second.lines().count(),
            1,
            "a retired server must receive no further pulls"
        );
    });
}

/// An expired pull must free the result ids, leave the cache untouched,
/// and retire the server so it is never pulled again (#533).
///
/// The retirement is what bounds the leak: a timed-out request cannot be
/// cancelled, so its entry in async-lsp's `outgoing` map is permanent.
/// Asking a stalled server once costs one entry; asking it on every
/// server-driven refresh would cost one per refresh, forever.
#[test]
fn an_expired_pull_frees_the_ids_and_retires_the_server() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (script, receipt) = mute_pull_server_script(&root);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    runtime.block_on(async {
        let config = ServerConfig {
            name: "mute-pull",
            command: python.to_string_lossy().into_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                receipt.to_string_lossy().into_owned(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        };
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let client = LspClient::spawn(
            &config,
            &root,
            build_client_capabilities(),
            &[],
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
        )
        .await
        .expect("the stub server handshakes");

        // SEEDED, so "the cache is untouched" is a claim that can fail:
        // against an empty cache the assertion below would hold whatever
        // the timeout path did to it.
        let result_ids = Arc::new(TokioMutex::new(DiagnosticResultIds::default()));
        result_ids
            .lock()
            .await
            .record("file:///seeded.py", Some("id-seeded"));
        let stalled = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = std_mpsc::channel();
        spawn_workspace_pull_within(
            vec![WorkspacePull {
                name: String::from("mute-pull"),
                client: Arc::new(TokioMutex::new(client)),
                identifier: None,
                result_ids: result_ids.clone(),
                stalled: stalled.clone(),
            }],
            tx,
            std::time::Duration::from_millis(200),
        );

        // Wait for the retirement rather than for a fixed duration: the
        // flag is the observable the production filter reads.
        let retired = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !stalled.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            retired.is_ok(),
            "an expired pull must retire the server, or every later \
                 refresh leaks another un-reapable request"
        );

        // The ids are free again, and the timeout recorded nothing: the
        // next pull re-sends the same `previous` set.
        let ids = tokio::time::timeout(std::time::Duration::from_secs(5), result_ids.lock())
            .await
            .expect("an expired pull must release the result ids");
        assert_eq!(
            ids.len(),
            1,
            "an expired pull must leave the result-id cache untouched, so \
                 the next pull re-sends the same `previous` set"
        );
    });
}

/// A server that accepts `workspace/diagnostic` and never answers it must
/// not wedge every later request to that client (#533 regression).
///
/// This drives the REAL [`spawn_workspace_pull`] against a real
/// [`LspClient`]. The pull previously held the client lock across the
/// unanswered await, and `open_doc` takes that same lock ON the worker
/// loop, so the loop stopped and Go to Definition did nothing at all from
/// 0.1.939 on. The assertion is that the client `Arc` is still lockable
/// while the pull is outstanding.
#[test]
fn a_hung_workspace_pull_leaves_the_client_lockable() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (script, receipt) = mute_pull_server_script(&root);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    runtime.block_on(async {
        let config = ServerConfig {
            name: "mute-pull",
            command: python.to_string_lossy().into_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                receipt.to_string_lossy().into_owned(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        };
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let client = LspClient::spawn(
            &config,
            &root,
            build_client_capabilities(),
            &[],
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
        )
        .await
        .expect("the stub server handshakes");

        // Precondition: the stub really does advertise the capability, so
        // `workspace_pull_targets` would select it in production. Without
        // this the test could pass by pulling nothing at all.
        assert!(
            workspace_diagnostics_supported(&client.capabilities().diagnostic_provider),
            "the stub must advertise workspaceDiagnostics, or no pull is issued"
        );

        let client = Arc::new(TokioMutex::new(client));
        let (tx, _rx) = std_mpsc::channel();
        spawn_workspace_pull(
            vec![WorkspacePull {
                name: String::from("mute-pull"),
                client: client.clone(),
                identifier: None,
                result_ids: Arc::new(TokioMutex::new(DiagnosticResultIds::default())),
                stalled: Arc::new(AtomicBool::new(false)),
            }],
            tx,
        );

        // Wait for the request to actually REACH the stub. A fixed sleep
        // would let the test pass for the wrong reason: `tokio::spawn`
        // need not have polled the pull future yet, and an unstarted pull
        // trivially holds no lock.
        let arrived = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !receipt.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            arrived.is_ok(),
            "the stub never received workspace/diagnostic, so this test \
                 would not exercise the lock at all"
        );

        // What `open_doc` does on the worker loop. Before the fix this
        // never returned, and with it went every queued command.
        let opened = tokio::time::timeout(std::time::Duration::from_secs(5), client.lock()).await;
        assert!(
            opened.is_ok(),
            "the workspace pull held the client across its unanswered \
                 await: open_doc — and every command behind it on the worker \
                 loop, Go to Definition included — blocks forever"
        );
    });
}

#[test]
fn a_workspace_pull_forwards_full_reports_and_leaves_unchanged_files_alone() {
    // The incrementality contract, end to end (#533). A Full report
    // reaches PROBLEMS and leaves its result id behind; an Unchanged one
    // is NOT re-processed but still records the id the server reissued;
    // a Full report with no id forgets the file, so the next pull cannot
    // send back an id the server never issued for this content.
    let (tx, rx) = std_mpsc::channel();
    let mut ids = DiagnosticResultIds::default();
    let uri = |p: &str| Url::from_file_path(p).expect("an absolute path");
    let wire = |message: &str| lsp_types::Diagnostic {
        range: def_range(3, 1),
        message: message.to_string(),
        ..Default::default()
    };
    let report = lsp_types::WorkspaceDiagnosticReport {
        items: vec![
            lsp_types::WorkspaceDocumentDiagnosticReport::Full(
                lsp_types::WorkspaceFullDocumentDiagnosticReport {
                    uri: uri("/ws/broken.toml"),
                    version: None,
                    full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                        result_id: Some("id-broken".into()),
                        items: vec![wire("unexpected key")],
                    },
                },
            ),
            lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(
                lsp_types::WorkspaceUnchangedDocumentDiagnosticReport {
                    uri: uri("/ws/steady.toml"),
                    version: None,
                    unchanged_document_diagnostic_report:
                        lsp_types::UnchangedDocumentDiagnosticReport {
                            result_id: "id-steady".into(),
                        },
                },
            ),
            lsp_types::WorkspaceDocumentDiagnosticReport::Full(
                lsp_types::WorkspaceFullDocumentDiagnosticReport {
                    uri: uri("/ws/fixed.toml"),
                    version: None,
                    full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                        result_id: None,
                        items: Vec::new(),
                    },
                },
            ),
        ],
    };
    let forwarded = apply_workspace_report("taplo", report.into(), &mut ids, &tx);
    assert_eq!(forwarded, 2, "both Full reports are the server's new word");

    let sent: Vec<DiagnosticsUpdate> = rx.try_iter().collect();
    let paths: Vec<&Path> = sent.iter().map(|u| u.path.as_path()).collect();
    assert_eq!(
        paths,
        vec![Path::new("/ws/broken.toml"), Path::new("/ws/fixed.toml")],
        "the Unchanged file must not be re-processed, and the CLEARED file must be: \
             an empty batch is how the app drops a fixed file's rows"
    );
    assert_eq!(sent[0].server, "taplo");
    assert_eq!(sent[0].diagnostics.len(), 1);
    assert!(sent[1].diagnostics.is_empty());

    let mut previous: Vec<(String, String)> = ids
        .previous()
        .into_iter()
        .map(|p| (p.uri.to_string(), p.value))
        .collect();
    previous.sort();
    assert_eq!(
        previous,
        vec![
            (
                "file:///ws/broken.toml".to_string(),
                "id-broken".to_string()
            ),
            (
                "file:///ws/steady.toml".to_string(),
                "id-steady".to_string()
            ),
        ],
        "the id-less report must leave nothing behind for /ws/fixed.toml"
    );
}

#[test]
fn client_capabilities_advertise_pull_diagnostics() {
    // The gate the whole of #533 stands on. Per LSP 3.17 a server
    // publishes `diagnosticProvider` only when the client declares
    // `textDocument.diagnostic`; without it a conforming server never
    // advertises the capability, so `workspace_diagnostics_supported`
    // reads `None` on every server and the pull path can never engage.
    // The detection landed first (#536), where the gap would have looked
    // like "no server supports it" rather than "we never asked".
    let caps = build_client_capabilities();
    let td = caps
        .text_document
        .as_ref()
        .expect("text document capabilities must be set");
    assert!(
        td.diagnostic.is_some(),
        "textDocument.diagnostic gates the server advertising diagnosticProvider at all"
    );
    // And the refresh half: a server sends `workspace/diagnostic/refresh`
    // only when the client says it can handle one, so without this the
    // re-pull after a project-wide change never arrives.
    let ws = caps
        .workspace
        .as_ref()
        .expect("workspace capabilities must be set");
    assert_eq!(
        ws.diagnostic
            .as_ref()
            .expect("workspace.diagnostic must be declared")
            .refresh_support,
        Some(true),
        "refreshSupport is what lets a server ask for a re-pull"
    );
}

#[test]
fn client_capabilities_offer_the_unnecessary_and_deprecated_tags() {
    let caps = build_client_capabilities();
    let publish = caps
        .text_document
        .and_then(|td| td.publish_diagnostics)
        .expect("publishDiagnostics must be declared");
    assert_eq!(
        publish.tag_support.map(|t| t.value_set),
        Some(vec![
            lsp_types::DiagnosticTag::UNNECESSARY,
            lsp_types::DiagnosticTag::DEPRECATED,
        ]),
        "a server that checks tagSupport would otherwise leave the tags out"
    );
    assert_eq!(
        publish.code_description_support,
        Some(true),
        "#1540: a server may leave a rule's link out otherwise"
    );
}

/// #1523: a related location takes one line of the hover, whatever
/// line breaks its message holds.
#[test]
fn a_related_location_label_is_one_line() {
    let related = RelatedLocation {
        path: PathBuf::from("/w/src/a.ts"),
        line: 1,
        character: 2,
        message: "'foo'\ris\r\ndeclared\nhere.".into(),
    };
    assert_eq!(
        related.label(Path::new("/w")),
        "src/a.ts(2, 3): 'foo' is declared here."
    );
}

#[test]
fn a_tagged_diagnostic_rides_back_to_code_actions_with_its_tags() {
    let d = |tags: DiagnosticTags| Diagnostic {
        start_line: 3,
        start_char: 4,
        end_line: 3,
        end_char: 7,
        severity: DiagnosticSeverity::Hint,
        message: "`tmp` is declared but its value is never read.".into(),
        tags,
        code: None,
        origin: None,
    };
    let unused = to_lsp_diagnostic(&d(DiagnosticTags {
        unnecessary: true,
        deprecated: false,
    }));
    assert_eq!(
        unused.tags,
        Some(vec![lsp_types::DiagnosticTag::UNNECESSARY]),
        "the server matches \"Remove unused declaration\" against the tag"
    );
    assert_eq!(
        to_lsp_diagnostic(&d(DiagnosticTags::default())).tags,
        None,
        "an untagged diagnostic sends no tags field"
    );
}

/// #1030: tsserver picks its quick fixes by the error `code`, ruff
/// reads its fix from `data`. Each server gets back its own diagnostics
/// exactly as it published them, and never another server's; one croft
/// made itself still goes to every server, as before.
#[test]
fn a_code_action_hands_each_server_its_own_diagnostics_unchanged() {
    let wire = |source: &str, code: &str| lsp_types::Diagnostic {
        range: def_range(0, 7),
        message: format!("{source} says {code}"),
        code: Some(lsp_types::NumberOrString::String(code.into())),
        source: Some(source.into()),
        data: Some(serde_json::json!({ "fix": code })),
        ..Default::default()
    };
    let ruff = crate::lsp::client::convert_diagnostic("ruff", &wire("Ruff", "F401"));
    let ty = crate::lsp::client::convert_diagnostic("ty", &wire("ty", "unresolved-import"));
    let ours = Diagnostic {
        origin: None,
        ..ruff.clone()
    };
    let all = [ruff, ty, ours.clone()];
    assert_eq!(
        code_action_diagnostics(&all, "ruff"),
        vec![wire("Ruff", "F401"), to_lsp_diagnostic(&ours)]
    );
    assert_eq!(
        code_action_diagnostics(&all, "ty"),
        vec![wire("ty", "unresolved-import"), to_lsp_diagnostic(&ours)]
    );
    assert_eq!(
        code_action_diagnostics(&all, "basedpyright"),
        vec![to_lsp_diagnostic(&ours)]
    );
}

fn def_range(line: u32, ch: u32) -> lsp_types::Range {
    let p = lsp_types::Position {
        line,
        character: ch,
    };
    lsp_types::Range { start: p, end: p }
}

#[test]
fn resolve_config_skips_a_server_not_on_path_and_keeps_one_that_is() {
    let absent = ServerConfig {
        name: "fake",
        command: "definitely-not-a-real-binary-zzzqqq".into(),
        args: vec![],
        language: Language::GO,
        initialization_options: None,
        provision: None,
    };
    assert!(
        resolve_config(&absent, false).is_none(),
        "a server whose command isn't on PATH must be skipped"
    );
    let present = ServerConfig {
        name: "shell",
        command: "sh".into(),
        args: vec![],
        language: Language::BASH,
        initialization_options: None,
        provision: None,
    };
    assert!(
        resolve_config(&present, false).is_some(),
        "a server whose command is on PATH must resolve"
    );
}

/// #930: a language whose only servers are missing says why, naming the
/// command and, for one croft could build, the tool it would need.
#[test]
fn a_language_with_no_installable_server_gets_a_note_naming_the_missing_command() {
    use crate::lsp::install::Provision;
    let nothing_missing = |_: &Provision| None;
    assert_eq!(
        missing_servers_note(
            Language::LUA,
            &[ServerConfig::lua_language_server()],
            nothing_missing
        )
        .as_deref(),
        Some(
            "Lua: no language server, so no diagnostics or completion (lua-language-server isn't on PATH)"
        )
    );
    assert_eq!(
        missing_servers_note(Language::GO, &[ServerConfig::gopls()], |_: &Provision| {
            Some("go")
        })
        .as_deref(),
        Some(
            "Go: no language server, so no diagnostics or completion (gopls isn't on PATH, and installing it needs go)"
        )
    );
    // An install that can run reports its own progress, so no note: gopls
    // with `go` present, or Python's ty beside a PATH-only basedpyright.
    assert_eq!(
        missing_servers_note(Language::GO, &[ServerConfig::gopls()], nothing_missing),
        None
    );
    let python = [ServerConfig::ty(), ServerConfig::basedpyright()];
    assert_eq!(
        missing_servers_note(Language::PYTHON, &python, nothing_missing),
        None
    );
    assert_eq!(
        missing_servers_note(Language::LUA, &[], nothing_missing),
        None
    );
}

/// #930: opening a file whose language has no server on PATH used to be
/// silent apart from lsp.log. It is now said once in OUTPUT › Server
/// Provisioning, not again for another file or another project root.
#[test]
fn a_language_left_without_a_server_is_reported_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let lang = Language("issue930-no-server");
    let mut registry = ServerRegistry::new();
    registry.register(
        lang,
        ServerConfig {
            name: "issue930-server",
            command: "issue930-not-a-real-server".into(),
            args: vec![],
            language: lang,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.ensure_clients(lang, &root));
    handle.block_on(state.ensure_clients(lang, &root));
    handle.block_on(state.ensure_clients(lang, &root.join("other")));
    let notes: Vec<String> = crate::output::snapshot(crate::output::CHANNEL_PROVISION)
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.text.contains("issue930-not-a-real-server"))
        .map(|l| l.text)
        .collect();
    assert_eq!(
        notes,
        [
            "issue930-no-server: no language server, so no diagnostics or completion (issue930-not-a-real-server isn't on PATH)"
        ],
        "the missing server must be reported exactly once"
    );
}

#[cfg(unix)]
#[test]
fn resolve_path_only_pins_a_fallback_dir_binary_to_its_absolute_path() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let bin = dir.join("rust-analyzer");
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = ServerConfig {
        name: "rust-analyzer",
        command: "rust-analyzer".into(),
        args: vec![],
        language: Language::RUST,
        initialization_options: None,
        provision: None,
    };

    // Not on PATH, but present in a fallback dir: must resolve to the
    // ABSOLUTE binary path (so a GUI-launched croft with a stripped PATH
    // still spawns it) and prepend that dir to the child's PATH.
    let (resolved, extra) = resolve_path_only(&cfg, false, std::slice::from_ref(&dir))
        .expect("a binary in a fallback dir must resolve");
    assert_eq!(resolved.command, bin.to_string_lossy());
    assert_eq!(extra, vec![dir.clone()]);

    // Already on PATH: spawn the bare command, no extra PATH entries.
    let (resolved, extra) = resolve_path_only(&cfg, true, std::slice::from_ref(&dir))
        .expect("an on-PATH server must resolve");
    assert_eq!(resolved.command, "rust-analyzer");
    assert!(extra.is_empty());

    // Neither on PATH nor in any fallback dir: skip.
    assert!(
        resolve_path_only(&cfg, false, &[]).is_none(),
        "a server missing everywhere must be skipped"
    );
}

/// A stand-in rustup in `dir/bin` with a `rust-analyzer` proxy linked to
/// it, returned as (rustup, proxy). Like the real one, it finds
/// `rust-analyzer` only for a project whose `rust-toolchain.toml` pins a
/// toolchain carrying it, names `stable` the active toolchain elsewhere,
/// and cannot name one where the pinned toolchain is not installed.
#[cfg(unix)]
fn fake_rustup(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let rustup = bin.join("rustup");
    let script = r#"#!/bin/sh
case "$1" in
which)
  [ "$RUSTUP_AUTO_INSTALL" = 0 ] || touch installed-a-toolchain
  if [ -f rust-toolchain.toml ]; then echo "$PWD/$2"; exit 0; fi
  echo "error: not a file" >&2
  exit 1 ;;
show)
  [ -f not-installed ] && exit 1
  echo "stable-ARCH-unknown-linux-gnu (default)" ;;
esac
"#;
    std::fs::write(&rustup, script.replace("ARCH", std::env::consts::ARCH)).unwrap();
    std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = bin.join("rust-analyzer");
    std::os::unix::fs::symlink("rustup", &proxy).unwrap();
    // Wait out "Text file busy" once: a test forking while the script
    // was open for writing holds it until that child execs.
    crate::review_ops::output_retrying_busy(std::process::Command::new(&rustup).arg("-V"))
        .expect("the stand-in rustup runs");
    (rustup, proxy)
}

#[cfg(unix)]
#[test]
fn a_rustup_proxy_is_told_apart_from_a_program_of_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let (rustup, proxy) = fake_rustup(tmp.path());
    let hard = tmp.path().join("bin/cargo-fmt");
    std::fs::hard_link(&rustup, &hard).unwrap();
    let elsewhere = tmp.path().join("rust-analyzer");
    std::os::unix::fs::symlink(&proxy, &elsewhere).unwrap();
    for (link, how) in [
        (proxy, "symlink"),
        (hard, "hard link"),
        (elsewhere, "link to a proxy"),
    ] {
        let found = rustup_behind(&link).unwrap_or_else(|| panic!("a {how} to rustup is a proxy"));
        assert_eq!(
            (found.file_name(), file_id(&found)),
            (rustup.file_name(), file_id(&rustup))
        );
    }
    let own = tmp.path().join("bin/clangd");
    std::fs::copy(&rustup, &own).unwrap();
    assert_eq!(
        rustup_behind(&own),
        None,
        "a binary of its own is never questioned"
    );
}

/// #927: rustup has a `rust-analyzer` proxy for every toolchain, so
/// whether it can start one is a question about the toolchain the project
/// selects, asked from its root. Where it cannot, croft must not spawn it.
#[cfg(unix)]
#[test]
fn a_rustup_proxy_without_the_component_is_not_the_server_croft_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let (rustup, proxy) = fake_rustup(tmp.path());
    let project = |name: &str, file: Option<&str>| {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        if let Some(file) = file {
            std::fs::write(dir.join(file), "").unwrap();
        }
        dir
    };
    let default = project("default", None);
    let pinned = project("pinned", Some("rust-toolchain.toml"));
    let uninstalled = project("uninstalled", Some("not-installed"));
    let name = "croft-test-927-rust-analyzer";
    let config = ServerConfig {
        name,
        command: proxy.to_string_lossy().into_owned(),
        args: vec![],
        language: Language::RUST,
        initialization_options: None,
        // No download for any platform, so croft's copy never lands here.
        provision: Some(crate::lsp::install::Provision::Binary {
            targets: &[],
            bin: "rust-analyzer",
            archive: crate::lsp::install::ArchiveKind::Gz,
            bin_path: None,
            termux_pkg: None,
            sha256: &[],
        }),
    };
    let rt = stub_runtime();
    let kept = rt.block_on(resolve_for_root(&config, &pinned, false));
    assert_eq!(
        kept.map(|(c, _)| c.command),
        Some(config.command.clone()),
        "a toolchain that has rust-analyzer keeps rustup's proxy, matching the user's compiler"
    );
    assert!(
        rt.block_on(resolve_for_root(&config, &default, false))
            .is_none(),
        "a proxy that can only print 'Unknown binary' must give way to croft's copy"
    );
    assert_eq!(
        crate::lsp::install::failure_status(name, "download failed".into()),
        "download failed; run: rustup component add rust-analyzer --toolchain stable",
        "if croft's copy cannot be had either, the status line says how to get rustup's, \
             for the toolchain the project selects whatever directory it is typed in"
    );
    assert_eq!(
        rt.block_on(rustup_lacks(&rustup, "rust-analyzer", &default)),
        Some(RustupLack {
            why: String::from("rustup's 'stable' toolchain has no rust-analyzer"),
            fix: String::from("rustup component add rust-analyzer --toolchain stable"),
        }),
    );
    // Ages a verdict past its recheck, as 30 s would.
    let backdate = |dir: &Path| {
        if let Ok(mut verdicts) = RUSTUP_VERDICTS.lock()
            && let Some((asked, _)) = verdicts.get_mut(&(name, dir.to_path_buf()))
            && let Some(due) = asked.checked_sub(RUSTUP_RECHECK)
        {
            *asked = due;
        }
    };
    // The user runs that command. A fresh verdict still holds; once it is
    // due a recheck, rustup's own server is the one croft starts, with no
    // restart.
    std::fs::write(default.join("rust-toolchain.toml"), "").unwrap();
    assert!(
        rt.block_on(resolve_for_root(&config, &default, false))
            .is_none()
    );
    backdate(&default);
    assert_eq!(
        rt.block_on(resolve_for_root(&config, &default, false))
            .map(|(c, _)| c.command),
        Some(config.command.clone()),
        "the component added since is found when the verdict is asked again"
    );
    assert_eq!(
        rt.block_on(rustup_lacks(&rustup, "rust-analyzer", &uninstalled)),
        None,
        "a toolchain that is not installed yet is the proxy's to install"
    );
    // A rustup that could not say (here the toolchain was not installed
    // yet) keeps the proxy, and is asked again too: the toolchain it
    // installed has no rust-analyzer.
    assert!(
        rt.block_on(resolve_for_root(&config, &uninstalled, false))
            .is_some()
    );
    std::fs::remove_file(uninstalled.join("not-installed")).unwrap();
    backdate(&uninstalled);
    assert!(
        rt.block_on(resolve_for_root(&config, &uninstalled, false))
            .is_none(),
        "an open question is asked again, and the proxy gives way"
    );
    assert!(
        [default, pinned, uninstalled]
            .iter()
            .all(|dir| !dir.join("installed-a-toolchain").exists()),
        "asking must never install a toolchain"
    );
}

#[test]
fn workspace_edits_reads_changes_map() {
    let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
    let mut changes = HashMap::new();
    changes.insert(
        uri,
        vec![TextEdit {
            range: def_range(2, 4),
            new_text: "renamed".to_string(),
        }],
    );
    let we = WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    let out = workspace_edits(&we);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, PathBuf::from("/tmp/foo.rs"));
    assert_eq!(out[0].1.len(), 1);
    assert_eq!(out[0].1[0].new_text, "renamed");
    assert_eq!(out[0].1[0].start, (2, 4));
}

#[test]
fn code_action_items_skips_disabled_and_normalises_edits() {
    use lsp_types::{CodeAction, CodeActionDisabled, CodeActionOrCommand, Command};
    let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
    let mut changes = HashMap::new();
    changes.insert(
        uri,
        vec![TextEdit {
            range: def_range(0, 0),
            new_text: "import os\n".to_string(),
        }],
    );
    let edit = WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    };
    let resp: CodeActionResponse = vec![
        // A bare command action (no inline edit; runs via executeCommand).
        CodeActionOrCommand::Command(Command {
            title: "Run fix".to_string(),
            command: "fix.run".to_string(),
            arguments: Some(vec![serde_json::json!(1)]),
        }),
        // A CodeAction carrying an inline workspace edit, marked preferred.
        CodeActionOrCommand::CodeAction(CodeAction {
            title: "Add import 'os'".to_string(),
            edit: Some(edit),
            is_preferred: Some(true),
            ..Default::default()
        }),
        // A disabled action: must be filtered out, mirroring VS Code's
        // lightbulb menu which hides disabled actions.
        CodeActionOrCommand::CodeAction(CodeAction {
            title: "Disabled thing".to_string(),
            disabled: Some(CodeActionDisabled {
                reason: "nope".to_string(),
            }),
            ..Default::default()
        }),
    ];
    let items = code_action_items(&resp, "ruff");
    assert_eq!(items.len(), 2, "the disabled action must be filtered out");
    assert_eq!(items[0].title, "Run fix");
    assert_eq!(
        items[0].server, "ruff",
        "each item is tagged with its server"
    );
    assert!(items[0].edits.is_empty());
    assert_eq!(items[0].command.as_ref().unwrap().command, "fix.run");
    assert!(!items[0].needs_resolve);
    assert_eq!(items[1].title, "Add import 'os'");
    assert!(items[1].is_preferred);
    assert_eq!(items[1].edits.len(), 1);
    assert_eq!(items[1].edits[0].1[0].new_text, "import os\n");
    assert!(!items[1].needs_resolve);
}

#[test]
fn code_action_items_flags_resolve_when_edit_absent() {
    use lsp_types::{CodeAction, CodeActionOrCommand};
    // ty / rust-analyzer style: the action arrives with `data` and no edit;
    // the edit is filled in by a later codeAction/resolve round trip.
    let resp: CodeActionResponse = vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: "Import symbol".to_string(),
        data: Some(serde_json::json!({"id": 7})),
        ..Default::default()
    })];
    let items = code_action_items(&resp, "basedpyright");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].server, "basedpyright");
    assert!(items[0].edits.is_empty());
    assert!(items[0].command.is_none());
    assert!(
        items[0].needs_resolve,
        "an action with data but no edit/command must be marked for resolve"
    );
}

#[test]
fn client_advertises_code_action_literal_and_resolve_support() {
    // Servers like ruff only return rich CodeAction objects (kinds such as
    // source.organizeImports / source.fixAll, plus resolvable edits) when the
    // client advertises codeActionLiteralSupport + resolveSupport. Without it
    // ty/ruff answer codeAction with nothing useful, so "Organize Imports"
    // never appears.
    let caps = build_client_capabilities();
    let ca = caps
        .text_document
        .and_then(|td| td.code_action)
        .expect("code action client capability must be declared");
    let kinds = ca
        .code_action_literal_support
        .expect("codeActionLiteralSupport must be declared")
        .code_action_kind
        .value_set;
    assert!(
        kinds.iter().any(|k| k == "source.organizeImports"),
        "the advertised kinds must include source.organizeImports"
    );
    assert!(
        kinds.iter().any(|k| k == "quickfix"),
        "the advertised kinds must include quickfix"
    );
    let resolves = ca
        .resolve_support
        .expect("resolveSupport must be declared so deferred edits resolve")
        .properties;
    assert!(resolves.iter().any(|p| p == "edit"));
    assert_eq!(ca.data_support, Some(true));
}

#[test]
fn code_action_capability_detection() {
    use lsp_types::{CodeActionOptions, CodeActionProviderCapability};
    assert!(
        !code_action_supported(&None),
        "absent provider is unsupported"
    );
    assert!(
        !code_action_supported(&Some(CodeActionProviderCapability::Simple(false))),
        "a bare false is unsupported"
    );
    assert!(code_action_supported(&Some(
        CodeActionProviderCapability::Simple(true)
    )));
    assert!(
        code_action_supported(&Some(CodeActionProviderCapability::Options(
            CodeActionOptions::default()
        ))),
        "an options object means supported"
    );
}

/// #1084: didOpen names a JSON-with-comments file `jsonc`, so the JSON
/// server neither flags its comments nor errors on trailing commas; plain
/// JSON stays `json`.
#[test]
fn json_with_comments_files_open_as_jsonc() {
    let jsonc = Language::from_lsp_id("jsonc").expect("jsonc is a language");
    for path in [
        "/repo/tsconfig.json",
        "/repo/.vscode/launch.json",
        "/repo/.babelrc",
        "/repo/notes.jsonc",
    ] {
        let lang = path_to_language(Path::new(path));
        assert_eq!(lang, Some(jsonc), "{path}");
        assert_eq!(lang.unwrap().lsp_id(), "jsonc");
    }
    let keybindings = crate::paths::config_dir().join("keybindings.json");
    assert_eq!(path_to_language(&keybindings), Some(jsonc));
    assert_eq!(
        path_to_language(Path::new("/repo/package.json")),
        Some(Language::JSON)
    );
}

#[test]
fn project_root_is_nearest_manifest_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // monorepo: root/app/svc is a Python sub-project with its own pyproject.
    let svc = root.join("app").join("svc");
    std::fs::create_dir_all(svc.join("src/pkg")).unwrap();
    std::fs::write(svc.join("pyproject.toml"), b"[project]\n").unwrap();
    let file = svc.join("src/pkg/mod.py");
    assert_eq!(project_root_within(&file, Language::PYTHON, root), svc);
}

#[test]
fn project_root_falls_back_to_workspace_root_without_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let file = root.join("a/b/loose.py");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    // No pyproject anywhere up the tree → workspace root.
    assert_eq!(project_root_within(&file, Language::PYTHON, root), root);
}

#[test]
fn project_root_ignores_manifest_of_other_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // A Cargo.toml must not anchor a Python file; Python keeps walking to root.
    std::fs::write(sub.join("Cargo.toml"), b"[package]\n").unwrap();
    let file = sub.join("script.py");
    assert_eq!(project_root_within(&file, Language::PYTHON, root), root);
    // ...but it does anchor a Rust file.
    let rs = sub.join("src/main.rs");
    std::fs::create_dir_all(rs.parent().unwrap()).unwrap();
    assert_eq!(project_root_within(&rs, Language::RUST, root), sub);
}

#[test]
fn declaration_capability_false_reads_as_unsupported() {
    // The bug: vtsls sends `declarationProvider: false`, which lsp-types
    // parses to `Some(DeclarationCapability::Simple(false))`. `Option::is_some`
    // read that as supported and croft called the unhandled method.
    assert!(!declaration_supported(&Some(
        DeclarationCapability::Simple(false)
    )));
    assert!(declaration_supported(&Some(DeclarationCapability::Simple(
        true
    ))));
    assert!(!declaration_supported(&None));
}

#[test]
fn one_of_capability_false_reads_as_unsupported() {
    let off: Option<OneOf<bool, ()>> = Some(OneOf::Left(false));
    let on: Option<OneOf<bool, ()>> = Some(OneOf::Left(true));
    let opts: Option<OneOf<bool, ()>> = Some(OneOf::Right(()));
    assert!(!one_of_supported(&off));
    assert!(one_of_supported(&on));
    assert!(one_of_supported(&opts));
    assert!(!one_of_supported(&None::<OneOf<bool, ()>>));
}

/// A `Full` report with no `result_id` must FORGET the file, not keep
/// the old id (#533).
///
/// Keeping it sends back an id the server never issued for the current
/// content; a server that trusts it answers `Unchanged` for a file that
/// has changed, and the diagnostics never refresh.
#[test]
fn a_report_without_a_result_id_forgets_the_file() {
    let mut ids = DiagnosticResultIds::default();
    assert!(ids.is_empty(), "cold to start");

    ids.record("file:///w/a.rs", Some("v1"));
    ids.record("file:///w/b.rs", Some("v1"));
    assert_eq!(ids.len(), 2);

    // b.rs comes back with no id: the server is declining incrementality
    // for it, so the stale id must go rather than being resent.
    ids.record("file:///w/b.rs", None);
    assert_eq!(ids.len(), 1, "the file without an id is forgotten");
    let sent: Vec<String> = ids
        .previous()
        .into_iter()
        .map(|p| p.uri.to_string())
        .collect();
    assert!(
        sent.iter().any(|u| u.contains("a.rs")),
        "the file that DID carry an id is still sent: {sent:?}"
    );
    assert!(
        !sent.iter().any(|u| u.contains("b.rs")),
        "the forgotten file is not sent: {sent:?}"
    );

    // A new id for the same file replaces rather than accumulating.
    ids.record("file:///w/a.rs", Some("v2"));
    assert_eq!(ids.len(), 1, "replacing an id does not add an entry");
    assert_eq!(
        ids.previous().first().map(|p| p.value.clone()),
        Some(String::from("v2")),
        "the newest id is what goes back"
    );
}

/// A server that pulls per-document but NOT per-workspace advertises
/// `diagnosticProvider` with `workspaceDiagnostics: false` (#533).
/// Reading the outer `Option` would send it a request it does not
/// implement; the inner flag is the only thing that separates them.
#[test]
fn workspace_diagnostics_reads_the_inner_flag_not_the_option() {
    let opts = |workspace: bool| lsp_types::DiagnosticOptions {
        identifier: None,
        inter_file_dependencies: false,
        workspace_diagnostics: workspace,
        work_done_progress_options: Default::default(),
    };

    // The trap: present, but per-document only.
    assert!(!workspace_diagnostics_supported(&Some(
        DiagnosticServerCapabilities::Options(opts(false))
    )));
    // Paired positive, so the assertion above cannot pass by the
    // predicate simply always saying no.
    assert!(workspace_diagnostics_supported(&Some(
        DiagnosticServerCapabilities::Options(opts(true))
    )));

    // The registration-options variant carries the same flag and must
    // not read as an unconditional yes.
    let reg = |workspace: bool| lsp_types::DiagnosticRegistrationOptions {
        text_document_registration_options: Default::default(),
        diagnostic_options: opts(workspace),
        static_registration_options: Default::default(),
    };
    assert!(!workspace_diagnostics_supported(&Some(
        DiagnosticServerCapabilities::RegistrationOptions(reg(false))
    )));
    assert!(workspace_diagnostics_supported(&Some(
        DiagnosticServerCapabilities::RegistrationOptions(reg(true))
    )));

    assert!(!workspace_diagnostics_supported(&None));
}

#[test]
fn hover_capability_false_reads_as_unsupported() {
    assert!(!hover_supported(&Some(HoverProviderCapability::Simple(
        false
    ))));
    assert!(hover_supported(&Some(HoverProviderCapability::Simple(
        true
    ))));
    assert!(!hover_supported(&None));
}

#[test]
fn type_definition_capability_false_reads_as_unsupported() {
    assert!(!type_definition_supported(&Some(
        TypeDefinitionProviderCapability::Simple(false)
    )));
    assert!(type_definition_supported(&Some(
        TypeDefinitionProviderCapability::Simple(true)
    )));
    assert!(!type_definition_supported(&None));
}

#[test]
fn implementation_capability_false_reads_as_unsupported() {
    assert!(!implementation_supported(&Some(
        ImplementationProviderCapability::Simple(false)
    )));
    assert!(implementation_supported(&Some(
        ImplementationProviderCapability::Simple(true)
    )));
    assert!(!implementation_supported(&None));
}

#[test]
fn reference_locations_maps_every_location() {
    // Go to References is 1:many: a symbol used in three places comes back
    // as a flat `Vec<Location>` (not a `GotoDefinitionResponse`), and every
    // one must survive the mapping, in order.
    let locs = vec![
        Location {
            uri: Url::from_file_path("/tmp/a.rs").unwrap(),
            range: def_range(2, 4),
        },
        Location {
            uri: Url::from_file_path("/tmp/b.rs").unwrap(),
            range: def_range(7, 1),
        },
        Location {
            uri: Url::from_file_path("/tmp/a.rs").unwrap(),
            range: def_range(11, 0),
        },
    ];
    assert_eq!(
        reference_locations(&locs),
        vec![
            (PathBuf::from("/tmp/a.rs"), 2, 4),
            (PathBuf::from("/tmp/b.rs"), 7, 1),
            (PathBuf::from("/tmp/a.rs"), 11, 0),
        ]
    );
}

#[test]
fn reference_locations_empty_is_empty() {
    assert!(reference_locations(&[]).is_empty());
}

#[test]
fn def_location_reads_scalar() {
    let uri = Url::from_file_path("/tmp/foo.rs").unwrap();
    let resp = GotoDefinitionResponse::Scalar(Location {
        uri,
        range: def_range(3, 5),
    });
    assert_eq!(
        def_location(&resp),
        Some((PathBuf::from("/tmp/foo.rs"), 3, 5))
    );
}

#[test]
fn def_location_reads_first_of_array() {
    let resp = GotoDefinitionResponse::Array(vec![
        Location {
            uri: Url::from_file_path("/tmp/a.rs").unwrap(),
            range: def_range(1, 0),
        },
        Location {
            uri: Url::from_file_path("/tmp/b.rs").unwrap(),
            range: def_range(9, 9),
        },
    ]);
    assert_eq!(
        def_location(&resp),
        Some((PathBuf::from("/tmp/a.rs"), 1, 0))
    );
}

#[test]
fn def_location_reads_link_target_selection_range() {
    let resp = GotoDefinitionResponse::Link(vec![LocationLink {
        origin_selection_range: None,
        target_uri: Url::from_file_path("/tmp/c.rs").unwrap(),
        target_range: def_range(10, 0),
        target_selection_range: def_range(12, 4),
    }]);
    assert_eq!(
        def_location(&resp),
        Some((PathBuf::from("/tmp/c.rs"), 12, 4))
    );
}

#[test]
fn def_location_is_none_for_empty_array() {
    let resp = GotoDefinitionResponse::Array(vec![]);
    assert_eq!(def_location(&resp), None);
}

#[test]
fn def_locations_reads_every_entry_of_array() {
    // Go to Implementations is 1:many: a trait with two implementors comes
    // back as an Array of both, and `def_locations` must keep both (whereas
    // `def_location` keeps only the first).
    let resp = GotoDefinitionResponse::Array(vec![
        Location {
            uri: Url::from_file_path("/tmp/a.rs").unwrap(),
            range: def_range(1, 0),
        },
        Location {
            uri: Url::from_file_path("/tmp/b.rs").unwrap(),
            range: def_range(9, 9),
        },
    ]);
    assert_eq!(
        def_locations(&resp),
        vec![
            (PathBuf::from("/tmp/a.rs"), 1, 0),
            (PathBuf::from("/tmp/b.rs"), 9, 9),
        ]
    );
}

#[test]
fn def_locations_collapses_scalar_to_one_entry() {
    let resp = GotoDefinitionResponse::Scalar(Location {
        uri: Url::from_file_path("/tmp/foo.rs").unwrap(),
        range: def_range(3, 5),
    });
    assert_eq!(
        def_locations(&resp),
        vec![(PathBuf::from("/tmp/foo.rs"), 3, 5)]
    );
}

#[test]
fn def_locations_reads_all_links() {
    let resp = GotoDefinitionResponse::Link(vec![
        LocationLink {
            origin_selection_range: None,
            target_uri: Url::from_file_path("/tmp/c.rs").unwrap(),
            target_range: def_range(10, 0),
            target_selection_range: def_range(12, 4),
        },
        LocationLink {
            origin_selection_range: None,
            target_uri: Url::from_file_path("/tmp/d.rs").unwrap(),
            target_range: def_range(20, 0),
            target_selection_range: def_range(22, 2),
        },
    ]);
    assert_eq!(
        def_locations(&resp),
        vec![
            (PathBuf::from("/tmp/c.rs"), 12, 4),
            (PathBuf::from("/tmp/d.rs"), 22, 2),
        ]
    );
}

#[test]
fn hover_text_reads_plain_markup() {
    let c = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: "fn foo(x: i32) -> i32".into(),
    });
    assert_eq!(hover_text(&c).as_deref(), Some("fn foo(x: i32) -> i32"));
}

#[test]
fn hover_text_reads_scalar_language_string() {
    let c = HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
        language: "rust".into(),
        value: "fn foo()".into(),
    }));
    assert_eq!(hover_text(&c).as_deref(), Some("fn foo()"));
}

#[test]
fn hover_text_reads_scalar_plain_string() {
    let c = HoverContents::Scalar(MarkedString::String("just text".into()));
    assert_eq!(hover_text(&c).as_deref(), Some("just text"));
}

#[test]
fn hover_text_joins_array_entries_and_skips_blanks() {
    let c = HoverContents::Array(vec![
        MarkedString::String("line one".into()),
        MarkedString::String("   ".into()),
        MarkedString::LanguageString(LanguageString {
            language: "rust".into(),
            value: "line two".into(),
        }),
    ]);
    assert_eq!(hover_text(&c).as_deref(), Some("line one\n\nline two"));
}

#[test]
fn hover_text_is_none_when_blank() {
    let c = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::PlainText,
        value: "   ".into(),
    });
    assert_eq!(hover_text(&c), None);
}

#[test]
fn hover_text_is_none_for_empty_array() {
    let c = HoverContents::Array(vec![]);
    assert_eq!(hover_text(&c), None);
}

fn drain_completion_blocking(manager: &LspManager, timeout: Duration) -> Option<CompletionResult> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(r) = manager.drain_completion() {
            return Some(r);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `python3` the stub server runs under, if there is one.
fn python_for_stub_server() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|entry| entry.join("python3"))
        .find(|candidate| is_executable_file(candidate))
}

fn any_python_completion_server_on_path() -> bool {
    is_on_path("basedpyright-langserver") || is_on_path("pyright-langserver") || is_on_path("ty")
}

#[test]
fn manager_open_change_close_pipe_via_ruff() {
    if !is_on_path("ruff") {
        eprintln!("SKIPPED: ruff not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "x = 1\n").expect("write demo");

    let manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), String::from("x = 1\n"));
    std::thread::sleep(Duration::from_millis(800));
    manager.change_doc(file.clone(), String::from("x = 2\n"));
    std::thread::sleep(Duration::from_millis(800));
    manager.close_doc(file.clone());
    std::thread::sleep(Duration::from_millis(400));
    drop(manager);
}

#[test]
fn manager_completion_against_python_lsp() {
    if !any_python_completion_server_on_path() {
        eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    let text = String::from("def f(input, num):\n    input_split = input.split()\n    inp\n");
    std::fs::write(&file, &text).expect("write demo");

    let mut manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), text);
    std::thread::sleep(Duration::from_millis(2500));

    let id = manager.request_completion(file.clone(), 2, 7, CompletionTrigger::Invoked);
    // 30s: basedpyright is a Node.js process that initialises slowly
    // under heavy parallel `cargo test` load (the suite spawns ~900
    // tests, many of which open PTYs). 10s was reliable in isolation
    // but flaked under that load; 30s leaves plenty of headroom
    // without slowing the happy path (the loop returns the moment
    // the completion arrives).
    let result =
        drain_completion_blocking(&manager, Duration::from_secs(30)).expect("completion arrived");
    assert_eq!(result.request_id, id);
    assert_eq!(result.path, file);
    let labels: Vec<&str> = result.items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| l.starts_with("input")),
        "expected at least one item starting with `input`, got: {labels:?}"
    );
}

/// A rename for a file no server tracks (a tab opened by a relative
/// path, #1193) is answered with no edits instead of never, which left
/// "Renaming symbol" on the status bar forever.
#[test]
fn a_rename_for_an_untracked_file_is_answered_with_no_edits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let mut manager = LspManager::new(root).expect("manager");
    let id = manager.request_rename(PathBuf::from("pkg/util.py"), 0, 4, "grand_total".into());
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        if let Some(r) = manager.drain_rename() {
            break Some(r);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let result = result.expect("the rename is answered");
    assert_eq!(result.request_id, id);
    assert!(result.edits.is_none());
}

fn drain_hover_blocking(manager: &LspManager, timeout: Duration) -> Option<HoverResult> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(r) = manager.drain_hover() {
            return Some(r);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn manager_hover_against_python_lsp() {
    if !any_python_completion_server_on_path() {
        eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    let text = String::from("def greet(name):\n    return 'hi ' + name\n\ngreet('x')\n");
    std::fs::write(&file, &text).expect("write demo");

    let mut manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), text);
    std::thread::sleep(Duration::from_millis(2500));

    let id = manager.request_hover(file.clone(), 3, 0);
    let result = drain_hover_blocking(&manager, Duration::from_secs(30)).expect("hover arrived");
    assert_eq!(result.request_id, id);
    let hover = result.text.expect("hover text for the greet call");
    assert!(
        hover.contains("greet"),
        "hover over the greet call should name the function, got: {hover:?}"
    );
}

fn drain_definition_blocking(manager: &LspManager, timeout: Duration) -> Option<DefinitionResult> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(r) = manager.drain_definition() {
            return Some(r);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn manager_definition_against_python_lsp() {
    if !any_python_completion_server_on_path() {
        eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    let text = String::from("def greet(name):\n    return name\n\ngreet('x')\n");
    std::fs::write(&file, &text).expect("write demo");

    let mut manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), text);
    std::thread::sleep(Duration::from_millis(2500));

    let id = manager.request_definition(file.clone(), 3, 0);
    let result =
        drain_definition_blocking(&manager, Duration::from_secs(30)).expect("definition arrived");
    assert_eq!(result.request_id, id);
    let (target_path, target_line, _col) = result.target.expect("definition target found");
    assert_eq!(target_path, file, "greet is defined in the same file");
    assert_eq!(target_line, 0, "greet is defined on line 0");
}

fn semantic_caps(range: Option<bool>) -> ServerCapabilities {
    ServerCapabilities {
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: vec![SemanticTokenType::PARAMETER],
                    token_modifiers: vec![],
                },
                range,
                full: Some(SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: Default::default(),
            },
        )),
        ..Default::default()
    }
}

#[test]
fn range_support_detected_only_when_advertised() {
    // ty advertises range; basedpyright advertises full-only.
    assert!(semantic_tokens_range_supported(&semantic_caps(Some(true))));
    assert!(!semantic_tokens_range_supported(&semantic_caps(None)));
    assert!(!semantic_tokens_range_supported(&semantic_caps(Some(
        false
    ))));
    assert!(!semantic_tokens_range_supported(
        &ServerCapabilities::default()
    ));
}

fn drain_semantic_blocking(
    manager: &LspManager,
    timeout: Duration,
) -> Option<SemanticTokensUpdate> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(r) = manager.drain_semantic_tokens() {
            return Some(r);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn manager_semantic_tokens_against_python_lsp() {
    if !any_python_completion_server_on_path() {
        eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    // `value` is a parameter referenced in the body; tree-sitter alone
    // cannot know the body use is the parameter, the LSP can.
    let text = String::from("def calc(value):\n    return value + 1\n");
    std::fs::write(&file, &text).expect("write demo");

    let manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), text.clone());
    std::thread::sleep(Duration::from_millis(2500));
    manager.request_semantic_tokens(file.clone(), 0);

    let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
        .expect("semantic tokens arrived");
    assert_eq!(update.path, file);
    assert!(
        update.legend.iter().any(|t| t == "parameter"),
        "the server legend should include the `parameter` token type, got: {:?}",
        update.legend
    );
    assert!(!update.data.is_empty(), "expected a non-empty token batch");

    // Decode and confirm the `value` reference in the body (line 1)
    // carries the parameter color, end-to-end through the real server.
    let line_starts = crate::highlight::compute_line_starts(text.as_bytes());
    let spans = crate::highlight::decode_semantic_tokens(
        &update.data,
        &update.legend,
        text.as_bytes(),
        &line_starts,
    );
    let body = "    return value + 1";
    let col = body.find("value").expect("value in body");
    let hit = spans[1]
        .iter()
        .find(|s| s.range().contains(&col))
        .expect("a semantic span should cover the body `value`");
    assert_eq!(
        hit.style.fg,
        Some(ratatui::style::Color::Rgb(0xd0, 0x87, 0x70)),
        "the parameter referenced in the body must carry the parameter color"
    );
}

#[test]
fn manager_semantic_tokens_range_against_python_lsp() {
    // The viewport-range first-paint path: a `semanticTokens/range` query
    // over the opening rows must return tokens and arrive flagged
    // `is_full == false`. Requires a range-capable server (ty); when only
    // full-only basedpyright is present the request yields nothing, so the
    // assertion is gated on ty being on PATH.
    if !is_on_path("ty") {
        eprintln!("SKIPPED: ty (range-capable) not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("range_demo.py");
    let text = String::from("def calc(value):\n    return value + 1\n");
    std::fs::write(&file, &text).expect("write range_demo");

    let manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), text.clone());
    std::thread::sleep(Duration::from_millis(2500));
    manager.request_semantic_tokens_range(file.clone(), 0, rows_end(&text, 2), 0);

    let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
        .expect("range semantic tokens arrived");
    assert_eq!(update.path, file);
    assert!(
        !update.is_full,
        "a range reply must be flagged is_full=false so it cannot clobber the full batch"
    );
    assert!(
        !update.data.is_empty(),
        "the viewport range must carry tokens for the visible function"
    );
}

// Reproduces the FULL real-app path for a file with a stray lone `\r`
// (the n-gram.py bug): opened through `Editor::open`, synced the way
// `sync_lsp` does (`lines.join("\n")`), then the batch applied via
// `Editor::apply_semantic_tokens`. The lone `\r` is a line break for the
// LSP but not for Rust's `str::lines`, so before the fix every token
// below it landed one row off and the body parameter went uncolored.
#[test]
fn editor_overlay_aligns_with_lsp_across_a_lone_cr() {
    if !any_python_completion_server_on_path() {
        eprintln!("SKIPPED: no basedpyright/pyright/ty on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("mixed.py");
    // A lone `\r` (the `\n\r` after line 0) sits above the function, the
    // same shape as the real n-gram.py. Buffer lines after the fix:
    //   0:"x = 1"  1:""  2:"def calc(value):"  3:"    return value + 1"
    let disk = "x = 1\n\rdef calc(value):\n    return value + 1\n";
    std::fs::write(&file, disk).expect("write mixed");

    let mut editor = crate::widgets::editor::Editor::new();
    editor.open(&file).expect("open");
    // The lone CR must split a line so our numbering matches the server's.
    assert_eq!(
        editor.lines_for_test().len(),
        4,
        "lone CR must split a line"
    );
    let sent = editor.lines_for_test().join("\n");

    let manager = LspManager::new(root).expect("manager");
    manager.open_doc(file.clone(), sent);
    std::thread::sleep(Duration::from_millis(2500));
    manager.request_semantic_tokens(file.clone(), 0);
    let update = drain_semantic_blocking(&manager, Duration::from_secs(30))
        .expect("semantic tokens arrived");

    // When ty is installed, the range-capable preference must route the
    // request to ty (instant on any workspace), not full-only basedpyright.
    // ty's legend uniquely carries `selfParameter`; basedpyright's does not.
    if is_on_path("ty") {
        assert!(
            update.legend.iter().any(|t| t == "selfParameter"),
            "ty (range-capable) must be preferred for semantic tokens; legend={:?}",
            update.legend
        );
    }

    editor.apply_semantic_tokens(update.path, update.data, update.legend, update.is_full);
    let overlay = editor.semantic_overlay_for_test();
    // The `value` reference in the body is on buffer line 3. It must carry
    // the parameter color there, and NOT bleed onto the wrong row.
    let orange = ratatui::style::Color::Rgb(0xd0, 0x87, 0x70);
    let colored_on = |line: usize| {
        overlay
            .get(line)
            .map(|s| s.iter().any(|sp| sp.style.fg == Some(orange)))
            .unwrap_or(false)
    };
    assert!(
        colored_on(3),
        "body parameter must be colored on its real row (line 3); overlay={overlay:?}"
    );
}

/// A formatting request the worker cannot route MUST still answer, or the
/// app's format-on-save latch stays armed forever: `save()` defers the
/// write until a reply lands, so a silent bail means Cmd+S wrote nothing
/// and said nothing. The per-LANGUAGE capability map that gates the
/// feature is true for any file of that language, including ones this
/// worker never tracked (outside the project root, failed didOpen).
#[test]
fn an_unroutable_format_request_still_answers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (fmt_tx, fmt_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.py");

    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_formatting(7, never_opened.clone(), 4, true, &fmt_tx)
            .await;
    });

    let reply = fmt_rx
        .try_recv()
        .expect("an untracked document must still get a reply");
    assert_eq!(reply.request_id, 7);
    assert_eq!(reply.path, never_opened);
    assert!(
        reply.unsupported,
        "the reply must tell the app no formatter is reachable"
    );
}

#[test]
fn prepare_rename_capability_needs_an_options_shaped_provider() {
    assert!(!prepare_rename_supported(&None));
    assert!(!prepare_rename_supported(&Some(OneOf::Left(true))));
    assert!(!prepare_rename_supported(&Some(OneOf::Right(
        lsp_types::RenameOptions {
            prepare_provider: None,
            work_done_progress_options: Default::default(),
        }
    ))));
    assert!(prepare_rename_supported(&Some(OneOf::Right(
        lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        }
    ))));
}

/// The rename prompt defers on this verdict, so an unroutable
/// prepare request must still answer (#254) — with `unsupported`,
/// which routes the app to the word-under-cursor fallback.
#[test]
fn an_unroutable_prepare_rename_answers_unsupported() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (prep_tx, prep_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.py");
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_prepare_rename(21, never_opened.clone(), 0, 0, &prep_tx)
            .await;
    });
    let reply = prep_rx.try_recv().expect("must answer");
    assert_eq!(reply.request_id, 21);
    assert!(reply.unsupported);
    assert!(reply.error.is_none() && reply.target.is_none());
}

/// A stub server for prepareRename (#1677). `mode` decides its answer:
/// `mute` never answers (the bug), `range` answers a range with a
/// placeholder, `error` refuses with a message.
fn prepare_rename_server_script(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("prepare_rename_server.py");
    std::fs::write(
        &path,
        r#"
import json, sys

mode = sys.argv[1]

def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def write(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        write({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {
            "renameProvider": {"prepareProvider": True},
        }}})
    elif method == "textDocument/prepareRename":
        if mode == "range":
            write({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "range": {"start": {"line": 0, "character": 4},
                          "end": {"line": 0, "character": 9}},
                "placeholder": "total",
            }})
        elif mode == "error":
            write({"jsonrpc": "2.0", "id": msg["id"], "error": {
                "code": -32803, "message": "cannot rename a keyword",
            }})
    elif method == "shutdown":
        write({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif "id" in msg:
        write({"jsonrpc": "2.0", "id": msg["id"], "result": None})
"#,
    )
    .expect("write stub server");
    path
}

/// Run one prepareRename against the stub in `mode`, bounded by
/// `timeout`, and return the verdict and how long it took to arrive.
/// `None` when python3 is missing.
fn prepare_rename_against(
    mode: &str,
    timeout: std::time::Duration,
    stalled: Arc<AtomicBool>,
) -> Option<(PrepareRenameResult, std::time::Duration)> {
    let python = python_for_stub_server()?;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let script = prepare_rename_server_script(&root);
    let file = root.join("calc.py");
    std::fs::write(&file, "def total(items):\n    return sum(items)\n").unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let (tx, rx) = std_mpsc::channel();
    let started = runtime.block_on(async {
        let config = ServerConfig {
            name: "prepare-rename",
            command: python.to_string_lossy().into_owned(),
            args: vec![script.to_string_lossy().into_owned(), mode.to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        };
        let (diag_tx, _diag_rx) = std_mpsc::channel();
        let (prog_tx, _prog_rx) = std_mpsc::channel();
        let client = LspClient::spawn(
            &config,
            &root,
            build_client_capabilities(),
            &[],
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            diag_tx,
            prog_tx,
        )
        .await
        .expect("the stub server handshakes");
        assert!(
            prepare_rename_supported(&client.capabilities().rename_provider),
            "the stub must advertise prepareProvider, or F2 never asks it"
        );
        let started = std::time::Instant::now();
        spawn_prepare_rename(
            PrepareRenameAsk {
                request_id: 7,
                server_name: String::from("prepare-rename"),
                client: Arc::new(TokioMutex::new(client)),
                uri: Url::from_file_path(&file).unwrap(),
                line: 0,
                character: 5,
                stalled,
            },
            timeout,
            &tx,
        );
        started
    });
    let reply = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("prepareRename must always answer");
    let took = started.elapsed();
    drop(runtime);
    Some((reply, took))
}

/// The bug (#1677): a server that never answers prepareRename left F2 on
/// "Preparing rename…" for good. Past the deadline the verdict is
/// `unsupported`, which opens the word-under-cursor prompt.
#[test]
fn a_prepare_rename_the_server_never_answers_falls_back_after_the_deadline() {
    let deadline = std::time::Duration::from_millis(300);
    let stalled = Arc::new(AtomicBool::new(false));
    let Some((reply, took)) = prepare_rename_against("mute", deadline, stalled.clone()) else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    assert_eq!(reply.request_id, 7);
    assert!(
        reply.unsupported,
        "a timed-out prepare routes to the fallback prompt"
    );
    assert!(reply.error.is_none(), "a slow server is not a refusal");
    assert!(reply.target.is_none());
    assert!(
        took >= deadline,
        "the verdict must wait out the deadline, took {took:?}"
    );
    assert!(
        stalled.load(Ordering::Relaxed),
        "the expired request retires the server from prepareRename"
    );
}

/// A server retired by an earlier timeout is not asked again, so it
/// cannot leak another abandoned request: F2 goes straight to the
/// fallback prompt, without waiting out a deadline.
#[test]
fn a_retired_server_is_not_asked_to_prepare_a_rename_again() {
    let stalled = Arc::new(AtomicBool::new(true));
    let deadline = std::time::Duration::from_secs(20);
    let Some((reply, took)) = prepare_rename_against("range", deadline, stalled) else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    assert!(reply.unsupported, "a retired server routes to the fallback");
    assert!(
        reply.target.is_none(),
        "the answering server was never asked"
    );
    assert!(took < deadline, "no deadline is waited out, took {took:?}");
}

/// The deadline F2 runs with is short enough to feel like an answer and
/// long enough for a busy server.
#[test]
fn the_prepare_rename_deadline_is_a_couple_of_seconds() {
    assert!(PREPARE_RENAME_TIMEOUT >= std::time::Duration::from_secs(1));
    assert!(PREPARE_RENAME_TIMEOUT <= std::time::Duration::from_secs(5));
}

/// Negative: a server that answers in time keeps its range and
/// placeholder; the deadline only matters when no answer comes.
#[test]
fn a_prepare_rename_answered_in_time_keeps_the_servers_range() {
    let Some((reply, _)) = prepare_rename_against(
        "range",
        std::time::Duration::from_secs(20),
        Arc::new(AtomicBool::new(false)),
    ) else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    assert!(!reply.unsupported);
    assert!(reply.error.is_none());
    assert_eq!(
        reply.target,
        Some(((0, 4, 0, 9), Some(String::from("total"))))
    );
}

/// Negative: a server's refusal still fails fast with its own message,
/// rather than being mistaken for a timeout and falling back.
#[test]
fn a_prepare_rename_refusal_is_not_turned_into_the_fallback() {
    let Some((reply, took)) = prepare_rename_against(
        "error",
        std::time::Duration::from_secs(20),
        Arc::new(AtomicBool::new(false)),
    ) else {
        eprintln!("SKIPPED: no python3 on PATH");
        return;
    };
    assert!(
        !reply.unsupported,
        "a refusal must not open the fallback prompt"
    );
    let error = reply.error.unwrap_or_default();
    assert!(
        error.starts_with("cannot rename a keyword"),
        "the server's own reason is shown: {error}"
    );
    assert!(
        took < std::time::Duration::from_secs(20),
        "a refusal answers at once, not at the deadline"
    );
}

/// Format Selection keeps whole-document formatting's always-answer
/// contract (#254): an unroutable request still reports unsupported.
#[test]
fn an_unroutable_range_format_request_still_answers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (fmt_tx, fmt_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.py");
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_range_formatting(11, never_opened.clone(), (0, 0), (2, 0), 4, true, &fmt_tx)
            .await;
    });
    let reply = fmt_rx
        .try_recv()
        .expect("an untracked document must still get a reply");
    assert_eq!(reply.request_id, 11);
    assert!(reply.unsupported);
}

#[test]
fn linked_editing_capability_shapes_gate_correctly() {
    use lsp_types::LinkedEditingRangeServerCapabilities as L;
    assert!(!linked_editing_supported(&None));
    assert!(!linked_editing_supported(&Some(L::Simple(false))));
    assert!(linked_editing_supported(&Some(L::Simple(true))));
    assert!(linked_editing_supported(&Some(L::Options(
        lsp_types::LinkedEditingRangeOptions::default()
    ))));
}

/// #607: the first inline completion with text is taken, a snippet as
/// its plain text, and one whose range spans lines is skipped.
#[test]
fn the_first_usable_inline_completion_is_read_as_plain_text() {
    let item = |text: &str, lines: (u32, u32), snippet: bool| {
        serde_json::from_value::<lsp_types::InlineCompletionItem>(serde_json::json!({
            "insertText": text,
            "range": {
                "start": {"line": lines.0, "character": 0},
                "end": {"line": lines.1, "character": 3},
            },
            "insertTextFormat": if snippet { 2 } else { 1 },
        }))
        .unwrap()
    };
    let answer = lsp_types::InlineCompletionResponse::List(lsp_types::InlineCompletionList {
        items: vec![
            item("  ", (4, 4), false),
            item("two lines", (4, 5), false),
            item("foo(${1:x})$0", (4, 4), true),
        ],
    });
    assert_eq!(
        first_inline_item(answer),
        Some(InlineItem {
            text: String::from("foo(x)"),
            range: Some((4, 0, 4, 3)),
        })
    );
    let bare = serde_json::from_value(serde_json::json!([{"insertText": "bar"}])).unwrap();
    assert_eq!(
        first_inline_item(bare),
        Some(InlineItem {
            text: String::from("bar"),
            range: None,
        })
    );
}

#[test]
fn folding_range_capability_shapes_gate_correctly() {
    use lsp_types::FoldingRangeProviderCapability as F;
    assert!(!folding_range_supported(&None));
    assert!(!folding_range_supported(&Some(F::Simple(false))));
    assert!(folding_range_supported(&Some(F::Simple(true))));
    assert!(folding_range_supported(&Some(F::FoldingProvider(
        lsp_types::FoldingProviderOptions {}
    ))));
    assert!(folding_range_supported(&Some(F::Options(
        lsp_types::StaticTextDocumentColorProviderOptions {
            document_selector: None,
            id: None,
        }
    ))));
}

#[test]
fn normalise_folding_range_drops_empty_spans_and_maps_kinds() {
    let r = |start, end, kind| lsp_types::FoldingRange {
        start_line: start,
        start_character: None,
        end_line: end,
        end_character: None,
        kind,
        collapsed_text: None,
    };
    assert_eq!(normalise_folding_range(r(5, 5, None)), None);
    assert_eq!(normalise_folding_range(r(5, 4, None)), None);
    let got = normalise_folding_range(r(2, 9, Some(lsp_types::FoldingRangeKind::Region)))
        .expect("a real span survives");
    assert_eq!((got.start_line, got.end_line), (2, 9));
    assert_eq!(got.kind, FoldRangeKind::Region);
    assert_eq!(
        normalise_folding_range(r(0, 1, None)).map(|g| g.kind),
        Some(FoldRangeKind::Other)
    );
}

/// An unroutable folding-range request is SILENT — no reply at all —
/// which is the two-phase contract: the editor keeps its
/// indentation/region fallback unless a capable server answers.
#[test]
fn an_unroutable_folding_request_sends_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (fold_tx, fold_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.py");
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_folding_ranges(never_opened, 3, &fold_tx)
            .await;
    });
    assert!(
        fold_rx.try_recv().is_err(),
        "no reply may be sent for an untracked document"
    );
}

#[test]
fn selection_range_capability_shapes_gate_correctly() {
    use lsp_types::SelectionRangeProviderCapability as S;
    assert!(!selection_range_supported(&None));
    assert!(!selection_range_supported(&Some(S::Simple(false))));
    assert!(selection_range_supported(&Some(S::Simple(true))));
    assert!(selection_range_supported(&Some(S::Options(
        lsp_types::SelectionRangeOptions {
            work_done_progress_options: Default::default(),
        }
    ))));
}

#[test]
fn flatten_selection_chain_walks_parents_and_drops_repeats() {
    let range = |sl, sc, el, ec| lsp_types::Range {
        start: lsp_types::Position {
            line: sl,
            character: sc,
        },
        end: lsp_types::Position {
            line: el,
            character: ec,
        },
    };
    let chain = lsp_types::SelectionRange {
        range: range(1, 4, 1, 9),
        parent: Some(Box::new(lsp_types::SelectionRange {
            range: range(1, 4, 1, 9), // repeated link some servers emit
            parent: Some(Box::new(lsp_types::SelectionRange {
                range: range(0, 0, 3, 0),
                parent: None,
            })),
        })),
    };
    assert_eq!(
        flatten_selection_chain(&chain),
        vec![(1, 4, 1, 9), (0, 0, 3, 0)]
    );
}

/// An unroutable selectionRange request answers `unsupported` — the
/// gesture is waiting on the reply, and the verdict is what routes it
/// to the tree-sitter fallback instead of hanging.
#[test]
fn an_unroutable_selection_range_request_answers_unsupported() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (sel_tx, sel_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.py");
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_selection_ranges(9, never_opened.clone(), vec![(0, 0)], &sel_tx)
            .await;
    });
    let reply = sel_rx
        .try_recv()
        .expect("an untracked document must still get a verdict");
    assert_eq!(reply.request_id, 9);
    assert!(reply.unsupported);
    assert!(reply.chains.is_empty());
}

#[test]
fn color_capability_shapes_and_channel_scaling() {
    use lsp_types::ColorProviderCapability as C;
    assert!(!color_provider_supported(&None));
    assert!(!color_provider_supported(&Some(C::Simple(false))));
    assert!(color_provider_supported(&Some(C::Simple(true))));
    let ci = lsp_types::ColorInformation {
        range: lsp_types::Range {
            start: lsp_types::Position {
                line: 3,
                character: 7,
            },
            end: lsp_types::Position {
                line: 3,
                character: 14,
            },
        },
        color: lsp_types::Color {
            red: 1.0,
            green: 0.5,
            blue: 0.0,
            alpha: 1.0,
        },
    };
    let item = color_item(&ci);
    assert_eq!((item.line, item.character), (3, 7));
    assert_eq!((item.red, item.green, item.blue), (255, 128, 0));
    assert_eq!(item.raw.3, 1.0);
}

/// The presentation picker defers on the reply, so an unroutable
/// request must still answer — with an empty offering (#254).
#[test]
fn an_unroutable_color_presentation_request_answers_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let (color_tx, color_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry: ServerRegistry::new(),
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let never_opened = root.join("stray.css");
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .request_color_presentations(
                31,
                never_opened.clone(),
                (1.0, 0.0, 0.0, 1.0),
                (0, 0),
                (0, 7),
                &color_tx,
            )
            .await;
    });
    let reply = color_rx.try_recv().expect("must answer");
    assert_eq!(reply.request_id, 31);
    assert!(reply.presentations.is_empty());
}

/// Minimal LSP server that answers `initialize` and appends every
/// incoming method name to the log file given as argv[1]. Lets a test
/// assert exactly which notifications croft put on the wire.
///
/// It advertises the `CAPABILITIES` a test prepends to the script (none
/// by default), and a `didSave` line also carries the notification's
/// `text` as JSON, so a test can tell a bare save from one with the text.
const FAKE_LSP_RECORDER: &str = r#"
import json, sys

log = open(sys.argv[1], "a")

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if method == "textDocument/didSave":
        log.write(method + " " + json.dumps(msg["params"].get("text")) + "\n")
    else:
        log.write(method + "\n")
    log.flush()
    if "id" in msg:
        if method == "initialize":
            capabilities = globals().get("CAPABILITIES", {})
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": capabilities}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// #694: restarts are budgeted per (language, root), so a server that
/// dies on every start cannot respawn on every keystroke.
#[test]
fn server_restarts_are_budgeted_per_window() {
    let mut restarts = ServerRestarts::default();
    let key: ClientKey = (Language::PYTHON, PathBuf::from("/w"));
    let other: ClientKey = (Language::PYTHON, PathBuf::from("/other"));
    let t0 = Instant::now();
    for i in 0..MAX_RESTARTS {
        assert!(
            restarts.try_restart(&key, t0),
            "restart {i} is within budget"
        );
    }
    assert!(!restarts.try_restart(&key, t0), "one more is refused");
    assert!(restarts.gave_up.contains(&key));
    // The budget is per key: another root is unaffected.
    assert!(restarts.try_restart(&other, t0));
    // Attempts age out of the window.
    let mut fresh = ServerRestarts::default();
    for _ in 0..MAX_RESTARTS {
        fresh.try_restart(&key, t0);
    }
    assert!(fresh.try_restart(&key, t0 + RESTART_WINDOW + Duration::from_secs(1)));
}

/// A stub rust-analyzer (#1626): answers `initialize`, logs each
/// method, and answers `rust-analyzer/expandMacro` with an expansion.
const FAKE_RUST_ANALYZER: &str = r#"
import json, sys

log = open(sys.argv[1], "a")

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    log.write(method + " " + json.dumps(msg.get("params")) + "\n")
    log.flush()
    if "id" in msg:
        if method == "initialize":
            result = {"capabilities": {}}
        elif method == "rust-analyzer/expandMacro":
            result = {"name": "sq", "expansion": "(2 + 1) * (2 + 1)"}
        else:
            result = None
        send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
    if method == "exit":
        break
"#;

/// A worker for `root` whose only Rust server is `name`, run as the
/// stub rust-analyzer logging to `log`.
fn worker_with_rust_server(
    python: &Path,
    root: &Path,
    name: &'static str,
    log: &Path,
) -> WorkerState {
    worker_with_rust_stub(python, root, name, log, FAKE_RUST_ANALYZER)
}

/// [`worker_with_rust_server`] with the stub `source` as the server.
fn worker_with_rust_stub(
    python: &Path,
    root: &Path,
    name: &'static str,
    log: &Path,
    source: &str,
) -> WorkerState {
    let script = root.join("fake_ra.py");
    std::fs::write(&script, source).expect("write fake server");
    let mut registry = ServerRegistry::new();
    registry.register(
        Language::RUST,
        ServerConfig {
            name,
            command: python.display().to_string(),
            args: vec![script.display().to_string(), log.display().to_string()],
            language: Language::RUST,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    WorkerState {
        workspace_root: root.to_path_buf(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    }
}

/// #1626: Expand Macro goes to rust-analyzer as its own method, with the
/// document and caret, and its raw answer comes back to the app.
#[cfg(unix)]
#[test]
fn a_rust_analyzer_request_reaches_it_and_its_answer_comes_back() {
    use crate::lsp::rust_analyzer::RaRequest;
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("write main.rs");
    let log = root.join("methods.log");
    let mut state = worker_with_rust_server(&python, &root, "rust-analyzer", &log);
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(file.clone(), String::from("fn main() {}\n")));
    let (tx, rx) = std_mpsc::channel();
    handle.block_on(state.request_rust_analyzer(
        7,
        file.clone(),
        RaRequest::ExpandMacro,
        (0, 3),
        &tx,
    ));
    let got = rx
        .recv_timeout(crate::test_budget::spawn_budget(Duration::from_secs(10)))
        .expect("an answer");
    assert_eq!(got.request_id, 7);
    assert_eq!(got.request, RaRequest::ExpandMacro);
    let value = got.outcome.expect("the stub answers");
    assert_eq!(
        crate::lsp::rust_analyzer::expansion(&value),
        Some((String::from("sq"), String::from("(2 + 1) * (2 + 1)")))
    );
    let sent = std::fs::read_to_string(&log).unwrap_or_default();
    let line = sent
        .lines()
        .find(|l| l.starts_with("rust-analyzer/expandMacro "))
        .unwrap_or_else(|| panic!("the method was sent:\n{sent}"));
    assert!(
        line.contains("\"line\": 0") && line.contains("\"character\": 3"),
        "{line}"
    );
}

/// #1626 negative: a Rust server that is not rust-analyzer is never sent
/// its methods; the request answers "not running" at once.
#[cfg(unix)]
#[test]
fn a_rust_analyzer_request_never_goes_to_another_server() {
    use crate::lsp::rust_analyzer::RaRequest;
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("write main.rs");
    let log = root.join("methods.log");
    let mut state = worker_with_rust_server(&python, &root, "other-rust-server", &log);
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(file.clone(), String::from("fn main() {}\n")));
    let (tx, rx) = std_mpsc::channel();
    handle.block_on(state.request_rust_analyzer(
        8,
        file.clone(),
        RaRequest::ExpandMacro,
        (0, 3),
        &tx,
    ));
    let got = rx.recv_timeout(Duration::from_secs(5)).expect("an answer");
    assert_eq!(
        got.outcome,
        Err(String::from("rust-analyzer is not running for this file"))
    );
    let sent = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!sent.contains("rust-analyzer/"), "{sent}");
    // And a file that is not open answers the same way.
    handle.block_on(state.request_rust_analyzer(
        9,
        root.join("other.rs"),
        RaRequest::ExpandMacro,
        (0, 0),
        &tx,
    ));
    let got = rx.recv_timeout(Duration::from_secs(5)).expect("an answer");
    assert!(got.outcome.is_err());
}

/// A stub server that, once initialized, registers a `**/*.rs` watcher
/// (#908). It logs each method with its params, and each reply croft
/// sends it as `reply <error>`.
const FAKE_LSP_WATCHING: &str = r#"
import json, sys

log = open(sys.argv[1], "a")

def read_msg():
    length = None
    while line := sys.stdin.buffer.readline().strip():
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    return None if length is None else json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n%s" % (len(body), body))
    sys.stdout.buffer.flush()

while (msg := read_msg()) is not None:
    method = msg.get("method")
    if method is None:
        log.write("reply " + json.dumps(msg.get("error")) + "\n")
    else:
        log.write(method + " " + json.dumps(msg.get("params")) + "\n")
    log.flush()
    if method == "initialized":
        watchers = [{"globPattern": "**/*.rs"}]
        registration = {"id": "rs", "method": "workspace/didChangeWatchedFiles",
                        "registerOptions": {"watchers": watchers}}
        send({"jsonrpc": "2.0", "id": "watch", "method": "client/registerCapability",
              "params": {"registrations": [registration]}})
    elif method and "id" in msg:
        result = {"capabilities": {}} if method == "initialize" else None
        send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
    if method == "exit":
        break
"#;

/// #908: a server that registered watchers hears about the files changed
/// on disk that they cover, a file that is gone as deleted, and nothing
/// about a document it has open, which reaches it through didChange.
#[cfg(unix)]
#[test]
fn disk_changes_reach_the_servers_watching_them() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let [open, lib, gone, notes] =
        ["main.rs", "lib.rs", "gone.rs", "notes.md"].map(|f| root.join(f));
    for f in [&open, &lib, &notes] {
        std::fs::write(f, "\n").expect("write file");
    }
    let log = root.join("methods.log");
    let mut state = worker_with_rust_stub(&python, &root, "rust-analyzer", &log, FAKE_LSP_WATCHING);
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(open.clone(), String::from("\n")));
    let sent = || std::fs::read_to_string(&log).unwrap_or_default();
    crate::test_budget::await_spawned(
        Duration::from_millis(500),
        "croft to accept the watcher registration",
        || sent().contains("reply null"),
    );
    let _in_runtime = handle.enter();
    let changed =
        [&open, &lib, &gone, &notes].map(|p| (p.clone(), lsp_types::FileChangeType::CHANGED));
    state.did_change_watched_files(changed.to_vec());
    let method = "workspace/didChangeWatchedFiles ";
    crate::test_budget::await_spawned(
        Duration::from_millis(500),
        "the watched-files notification",
        || sent().contains(method),
    );
    let log = sent();
    let line = log.lines().find_map(|l| l.strip_prefix(method));
    let uri = |p: &Path| Url::from_file_path(p).expect("uri").to_string();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(line.expect("sent")).expect("params"),
        serde_json::json!({ "changes": [
            { "uri": uri(&lib), "type": 2 },
            { "uri": uri(&gone), "type": 3 },
        ]})
    );
}

/// A worker whose python and go servers are fake recorders that log
/// their pid to `<root>/<lang>.pids` (#1621).
fn two_language_worker(root: &Path, python: &Path) -> (WorkerState, Arc<AtomicBool>) {
    let script = root.join("fake_lsp.py");
    std::fs::write(
        &script,
        format!(
            "import os, sys\nopen(sys.argv[2], 'a').write(str(os.getpid()) + '\\n')\n{FAKE_LSP_RECORDER}"
        ),
    )
    .expect("write fake server");
    let mut registry = ServerRegistry::new();
    for (lang, name, tag) in [
        (Language::PYTHON, "fake-python", "python"),
        (Language::GO, "fake-go", "go"),
    ] {
        registry.register(
            lang,
            ServerConfig {
                name,
                command: python.display().to_string(),
                args: vec![
                    script.display().to_string(),
                    root.join(format!("{tag}.log")).display().to_string(),
                    root.join(format!("{tag}.pids")).display().to_string(),
                ],
                language: lang,
                initialization_options: None,
                provision: None,
            },
        );
    }
    let restarted = Arc::new(AtomicBool::new(false));
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let state = WorkerState {
        workspace_root: root.to_path_buf(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::reporting_to(restarted.clone()),
    };
    (state, restarted)
}

/// The pids a fake server of `tag` has logged under `root`, waiting up to
/// ten seconds for `n` of them.
fn logged_pids(root: &Path, tag: &str, n: usize) -> Vec<i32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen: Vec<i32> = std::fs::read_to_string(root.join(format!("{tag}.pids")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        if seen.len() >= n || Instant::now() >= deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether process `pid` is gone, waiting up to ten seconds for it.
fn process_exits(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // SAFETY: signal 0 only asks whether the pid exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// #1621: "Restart for This File" stops the file's server and starts a
/// new one, even one croft had given up on, and leaves the servers of
/// other languages running.
#[cfg(unix)]
#[test]
fn restarting_a_file_s_servers_leaves_other_languages_running() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let py = root.join("demo.py");
    let go = root.join("main.go");
    std::fs::write(&py, "x = 1\n").unwrap();
    std::fs::write(&go, "package main\n").unwrap();
    let (mut state, restarted) = two_language_worker(&root, &python);
    let (diag_tx, diag_rx) = std_mpsc::channel();
    state.diagnostics_tx = diag_tx;
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    handle.block_on(state.open_doc(go.clone(), String::from("package main\n")));
    let first_py = logged_pids(&root, "python", 1);
    let first_go = logged_pids(&root, "go", 1);
    assert_eq!((first_py.len(), first_go.len()), (1, 1));
    // A key croft gave up on after a crash loop still restarts on request.
    let py_key: ClientKey = (Language::PYTHON, root.clone());
    state.restarts.gave_up.insert(py_key.clone());

    handle.block_on(state.restart_servers_for(&py));
    let py_pids = logged_pids(&root, "python", 2);
    assert_eq!(py_pids.len(), 2, "a new python server started");
    assert!(process_exits(py_pids[0]), "the old python server exited");
    assert!(
        restarted.load(Ordering::Relaxed),
        "the app re-opens its tabs"
    );
    assert!(!state.docs.contains_key(&py), "the new server never saw it");
    let withdrawn: Vec<DiagnosticsUpdate> = diag_rx.try_iter().collect();
    assert!(
        withdrawn
            .iter()
            .any(|u| u.path == py && u.server == "fake-python" && u.diagnostics.is_empty()),
        "the old server's diagnostics are withdrawn: {withdrawn:?}"
    );
    assert!(
        !withdrawn.iter().any(|u| u.path == go),
        "go keeps its diagnostics"
    );
    assert!(
        state.clients[&py_key]
            .iter()
            .all(|m| m.client.try_lock().is_ok_and(|c| !c.has_exited()))
    );
    assert!(!state.restarts.gave_up.contains(&py_key));

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        logged_pids(&root, "go", 1),
        first_go,
        "go was not restarted"
    );
    // SAFETY: signal 0 only asks whether the pid exists.
    assert_eq!(unsafe { libc::kill(first_go[0], 0) }, 0, "go still runs");
    assert!(state.docs.contains_key(&go), "go keeps its document");
}

/// #1621: a file whose document is not open restarts only the servers
/// of its own project root, not every root of its language.
#[cfg(unix)]
#[test]
fn restarting_for_an_unopened_file_restarts_only_its_root() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let (a, b) = (root.join("a"), root.join("b"));
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
    }
    let (in_a, in_b) = (a.join("x.py"), b.join("y.py"));
    let (mut state, _restarted) = two_language_worker(&root, &python);
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(in_a.clone(), String::from("x = 1\n")));
    let first_a = logged_pids(&root, "python", 1);
    handle.block_on(state.open_doc(in_b.clone(), String::from("y = 1\n")));
    let both = logged_pids(&root, "python", 2);
    assert_eq!((first_a.len(), both.len()), (1, 2));
    let first_b = both[1];
    // As when its open failed: the worker holds no document for it.
    state.docs.remove(&in_a);

    handle.block_on(state.restart_servers_for(&in_a));
    let pids = logged_pids(&root, "python", 3);
    assert_eq!(pids.len(), 3, "root a's server started again");
    assert!(process_exits(first_a[0]), "root a's old server exited");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(logged_pids(&root, "python", 3).len(), 3);
    // SAFETY: signal 0 only asks whether the pid exists.
    assert_eq!(unsafe { libc::kill(first_b, 0) }, 0, "root b still runs");
    assert!(state.docs.contains_key(&in_b), "root b keeps its document");
}

/// Negative (#1621): a file no server serves restarts nothing.
#[cfg(unix)]
#[test]
fn restarting_for_a_file_no_server_serves_restarts_nothing() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let py = root.join("demo.py");
    std::fs::write(&py, "x = 1\n").unwrap();
    let (mut state, restarted) = two_language_worker(&root, &python);
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    let first = logged_pids(&root, "python", 1);
    handle.block_on(state.restart_servers_for(&root.join("notes.txt")));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(logged_pids(&root, "python", 1), first);
    assert!(!restarted.load(Ordering::Relaxed));
    assert!(state.docs.contains_key(&py));
}

/// #1842: an extension added, removed or toggled mid-session hands the
/// worker a new registry. The servers of a language it changes stop, and
/// that language's next open starts what is registered now; a language
/// it leaves alone keeps its servers running. Each time the app is told
/// to re-open its tabs, since one the old registry served nothing for
/// was never opened here.
#[cfg(unix)]
#[test]
fn a_new_registry_restarts_only_the_languages_it_changes() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let py = root.join("demo.py");
    let go = root.join("main.go");
    std::fs::write(&py, "x = 1\n").unwrap();
    std::fs::write(&go, "package main\n").unwrap();
    let (mut state, restarted) = two_language_worker(&root, &python);
    let (diag_tx, diag_rx) = std_mpsc::channel();
    state.diagnostics_tx = diag_tx;
    let python_server = state.registry.for_language(Language::PYTHON)[0].clone();
    let go_server = state.registry.for_language(Language::GO)[0].clone();
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    handle.block_on(state.open_doc(go.clone(), String::from("package main\n")));
    let first_py = logged_pids(&root, "python", 1);
    let first_go = logged_pids(&root, "go", 1);
    assert_eq!((first_py.len(), first_go.len()), (1, 1));

    // Python's extension turned off: only go's server is registered.
    let mut go_only = ServerRegistry::new();
    go_only.register(Language::GO, go_server.clone());
    handle.block_on(state.replace_registry(go_only));
    assert!(process_exits(first_py[0]), "python's server stopped");
    assert!(
        restarted.swap(false, Ordering::Relaxed),
        "tabs go out again"
    );
    assert!(!state.docs.contains_key(&py));
    let withdrawn: Vec<DiagnosticsUpdate> = diag_rx.try_iter().collect();
    assert!(
        withdrawn
            .iter()
            .any(|u| u.path == py && u.server == "fake-python" && u.diagnostics.is_empty()),
        "its diagnostics are withdrawn: {withdrawn:?}"
    );
    assert!(!withdrawn.iter().any(|u| u.path == go), "go keeps its own");
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(logged_pids(&root, "python", 1), first_py, "none starts");
    assert!(!state.docs.contains_key(&py));

    // Turned back on: the next open starts it.
    let mut both = ServerRegistry::new();
    both.register(Language::PYTHON, python_server);
    both.register(Language::GO, go_server);
    handle.block_on(state.replace_registry(both));
    assert!(restarted.load(Ordering::Relaxed), "tabs go out again");
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    assert_eq!(logged_pids(&root, "python", 2).len(), 2, "python starts");
    assert!(state.docs.contains_key(&py));

    assert_eq!(logged_pids(&root, "go", 1), first_go, "go never restarted");
    // SAFETY: signal 0 only asks whether the pid exists.
    assert_eq!(unsafe { libc::kill(first_go[0], 0) }, 0, "go still runs");
    assert!(state.docs.contains_key(&go), "go keeps its document");
    handle.block_on(state.shutdown_all());
}

/// #1842: an added extension that maps an open file to a language of its
/// own takes the file off its old language's servers, though their set
/// did not change, so the app's re-open reaches the new language as an
/// open rather than as a change to the old one.
#[cfg(unix)]
#[test]
fn a_file_an_added_extension_claims_leaves_its_old_servers() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let py = root.join("demo.py");
    std::fs::write(&py, "x = 1\n").unwrap();
    let (mut state, _restarted) = two_language_worker(&root, &python);
    let same = |state: &WorkerState| {
        let mut r = ServerRegistry::new();
        for lang in [Language::PYTHON, Language::GO] {
            r.register(lang, state.registry.for_language(lang)[0].clone());
        }
        r
    };
    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    let first = logged_pids(&root, "python", 1);
    assert_eq!(first.len(), 1);

    // The worker runs on this thread here, so it reads this table.
    crate::lsp::languages::reload_with_user_sources(&[
        "id = \"snake\"\nname = \"Snake\"\napi_version = 1\n\n[[languages]]\nid = \"snake\"\nextensions = [\"py\"]\n",
    ]);
    let registry = same(&state);
    handle.block_on(state.replace_registry(registry));
    assert!(!state.docs.contains_key(&py), "off python's servers");
    let log = root.join("python.log");
    crate::test_budget::await_spawned(Duration::from_secs(5), "the didClose", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("textDocument/didClose"))
    });
    // SAFETY: signal 0 only asks whether the pid exists.
    assert_eq!(unsafe { libc::kill(first[0], 0) }, 0, "python still runs");
    handle.block_on(state.open_doc(py.clone(), String::from("x = 1\n")));
    assert!(!state.docs.contains_key(&py), "snake has no server");
    handle.block_on(state.shutdown_all());
}

/// #694: a language server the kernel kills (croft makes them its first
/// OOM victims) comes back on the next command, its documents are
/// forgotten, and the app is told to re-open its tabs.
#[cfg(unix)]
#[test]
fn a_killed_language_server_is_restarted() {
    let Some(python) = python_for_stub_server() else {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "x = 1\n").expect("write demo");
    let script = root.join("fake_lsp.py");
    let pids = root.join("pids.log");
    std::fs::write(
        &script,
        format!(
            "import os, sys\nopen(sys.argv[2], 'a').write(str(os.getpid()) + '\\n')\n{FAKE_LSP_RECORDER}"
        ),
    )
    .expect("write fake server");
    let log = root.join("methods.log");
    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-recorder",
            command: python.display().to_string(),
            args: vec![
                script.display().to_string(),
                log.display().to_string(),
                pids.display().to_string(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let restarted = Arc::new(AtomicBool::new(false));
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::reporting_to(restarted.clone()),
    };
    let key: ClientKey = (Language::PYTHON, root.clone());
    let started = |n: usize| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seen: Vec<i32> = std::fs::read_to_string(&pids)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            if seen.len() >= n || Instant::now() >= deadline {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let runtime = LspRuntime::new().expect("runtime");
    let handle = runtime.handle().clone();
    handle.block_on(state.open_doc(file.clone(), String::from("x = 1\n")));
    let first = started(1);
    assert_eq!(first.len(), 1, "the server must have started once");
    assert!(state.docs.contains_key(&file));

    // What the OOM killer does.
    // SAFETY: plain kill(2) on a pid this test's server wrote.
    unsafe {
        libc::kill(first[0], libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.clients[&key]
        .iter()
        .all(|m| m.client.try_lock().is_ok_and(|c| c.has_exited()))
    {
        assert!(
            Instant::now() < deadline,
            "the client never noticed the kill"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    handle.block_on(state.restart_dead_servers());
    let both = started(2);
    assert_eq!(both.len(), 2, "a new server must have been started");
    assert_ne!(both[0], both[1]);
    assert!(
        restarted.load(Ordering::Relaxed),
        "the app must be told to re-open its tabs"
    );
    assert!(
        !state.docs.contains_key(&file),
        "the new server never saw the old documents"
    );
    assert!(
        state.clients[&key]
            .iter()
            .all(|m| m.client.try_lock().is_ok_and(|c| !c.has_exited())),
        "the restarted client must be alive"
    );
    // A live server is left alone: no third start.
    restarted.store(false, Ordering::Relaxed);
    handle.block_on(state.restart_dead_servers());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(started(2).len(), 2);
    assert!(!restarted.load(Ordering::Relaxed));

    // The app re-sends every tab after a restart. The first open reaches
    // the new server as didOpen; a repeat is a change, not a second open.
    handle.block_on(state.open_doc(file.clone(), String::from("x = 1\n")));
    handle.block_on(state.open_doc(file.clone(), String::from("x = 2\n")));
    let deadline = Instant::now() + Duration::from_secs(10);
    let methods = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("textDocument/didChange") || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Both servers share the log; count only what the new one received.
    let new_server = methods.rsplit("initialize\n").next().unwrap_or_default();
    assert_eq!(
        new_server.matches("textDocument/didOpen").count(),
        1,
        "a document is opened once per server: {methods:?}"
    );
    assert!(new_server.contains("textDocument/didChange"), "{methods:?}");
    handle.block_on(state.shutdown_all());

    // A key that used up its restarts is not re-probed through an empty
    // client list either.
    state.restarts.gave_up.insert(key.clone());
    state.clients.insert(key.clone(), Vec::new());
    handle.block_on(state.ensure_clients(key.0, &key.1));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(started(2).len(), 2, "a given-up server must stay down");

    // An empty list that comes back (a restart whose spawn failed, then a
    // later re-probe that works) must tell the app too: it already sent
    // its tabs into the empty list, so the new server never saw them.
    state.restarts.gave_up.remove(&key);
    restarted.store(false, Ordering::Relaxed);
    handle.block_on(state.ensure_clients(key.0, &key.1));
    assert_eq!(started(3).len(), 3, "the empty list must be re-probed");
    assert!(
        restarted.load(Ordering::Relaxed),
        "a recovered empty list must make the app re-open its tabs"
    );
    handle.block_on(state.shutdown_all());

    // A restart that loses a server which ran before it is tried again as
    // a set, not left short a server until croft restarts. The flaky
    // server runs until its flag file exists, then fails at launch.
    let flag = root.join("flaky.off");
    let flaky = root.join("flaky_lsp.py");
    std::fs::write(
        &flaky,
        format!(
            "import os, sys\nif os.path.exists({:?}): sys.exit(1)\n{}",
            flag.display().to_string(),
            std::fs::read_to_string(&script).expect("read fake server")
        ),
    )
    .expect("write flaky server");
    state.registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-flaky",
            command: python.display().to_string(),
            args: vec![
                flaky.display().to_string(),
                log.display().to_string(),
                pids.display().to_string(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let kill_one = |state: &WorkerState, pid: i32| {
        // SAFETY: plain kill(2) on a pid this test's server wrote.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !state.clients[&key]
            .iter()
            .any(|m| m.client.try_lock().is_ok_and(|c| c.has_exited()))
        {
            assert!(
                Instant::now() < deadline,
                "the client never noticed the kill"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // A server that never came up is not one a restart lost: the others
    // are restarted once for their own crash, and then left alone.
    std::fs::write(&flag, "").expect("write flag");
    state.restarts.attempts.clear();
    state.clients.remove(&key);
    handle.block_on(state.ensure_clients(key.0, &key.1));
    let pid = *started(4).last().expect("recorder pid");
    assert_eq!(state.clients[&key].len(), 1, "only the recorder came up");
    kill_one(&state, pid);
    handle.block_on(state.restart_dead_servers());
    assert_eq!(started(5).len(), 5);
    assert!(
        !state.restarts.incomplete.contains_key(&key),
        "a server that never ran must not make the set incomplete"
    );
    handle.block_on(state.restart_dead_servers());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(started(0).len(), 5, "a live set is not restarted again");
    handle.block_on(state.shutdown_all());

    // Both run; one crashes and the flaky one does not come back.
    std::fs::remove_file(&flag).expect("remove flag");
    state.restarts.attempts.clear();
    state.clients.remove(&key);
    handle.block_on(state.ensure_clients(key.0, &key.1));
    let pid = *started(7).last().expect("server pid");
    assert_eq!(state.clients[&key].len(), 2, "both servers came up");
    assert!(!state.restarts.incomplete.contains_key(&key));
    std::fs::write(&flag, "").expect("write flag");
    kill_one(&state, pid);
    restarted.store(false, Ordering::Relaxed);
    handle.block_on(state.restart_dead_servers());
    assert_eq!(started(8).len(), 8);
    assert_eq!(state.clients[&key].len(), 1, "only the recorder came back");
    assert!(restarted.load(Ordering::Relaxed));
    assert!(state.restarts.incomplete.contains_key(&key));
    handle.block_on(state.restart_dead_servers());
    assert_eq!(started(9).len(), 9, "a short set must be restarted");
    assert!(
        state.restarts.incomplete.contains_key(&key),
        "a restart that comes back short again must stay marked"
    );

    // A restart that brought nothing back leaves an empty list, which
    // every request re-probes. Those re-probes spend the restart budget,
    // so the servers stop being launched once it is gone.
    state.restarts.incomplete.remove(&key);
    let before = started(0).len();
    for _ in 0..MAX_RESTARTS + 2 {
        state.clients.insert(key.clone(), Vec::new());
        handle.block_on(state.ensure_clients(key.0, &key.1));
    }
    let launched = started(0).len() - before;
    assert!(
        launched < MAX_RESTARTS + 2,
        "re-probes past the budget must not launch servers ({launched})"
    );
    assert!(state.restarts.gave_up.contains(&key));
    handle.block_on(state.shutdown_all());
}

/// The `initialize` params a language server is sent for a workspace at
/// `dir_name` under a temp dir, as JSON, with that workspace's path.
fn initialize_params_for_workspace(dir_name: &str) -> (serde_json::Value, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp
        .path()
        .canonicalize()
        .expect("canonicalize")
        .join(dir_name);
    std::fs::create_dir_all(&root).expect("workspace");
    let script = tmp.path().join("fake_lsp.py");
    let params = tmp.path().join("initialize.json");
    // The recorder's message plumbing, with a loop that saves the
    // initialize params instead of logging methods.
    let head = FAKE_LSP_RECORDER
        .split("\nwhile True:")
        .next()
        .expect("recorder head");
    std::fs::write(
        &script,
        format!(
            "{head}\n\
                 while True:\n    msg = read_msg()\n    if msg is None:\n        break\n\
                 \x20   if msg.get('method') == 'exit':\n        break\n\
                 \x20   if msg.get('method') == 'initialize':\n\
                 \x20       open(sys.argv[2], 'w').write(json.dumps(msg['params']))\n\
                 \x20       send({{'jsonrpc': '2.0', 'id': msg['id'], 'result': {{'capabilities': {{}}}}}})\n\
                 \x20   elif 'id' in msg:\n\
                 \x20       send({{'jsonrpc': '2.0', 'id': msg['id'], 'result': None}})\n"
        ),
    )
    .expect("write fake server");
    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-initialize",
            command: "python3".into(),
            args: vec![
                script.display().to_string(),
                tmp.path().join("methods.log").display().to_string(),
                params.display().to_string(),
            ],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let runtime = LspRuntime::new().expect("runtime");
    let py = root.join("main.py");
    std::fs::write(&py, "x = 1\n").expect("write py");
    runtime
        .handle()
        .clone()
        .block_on(state.open_doc(py, String::from("x = 1\n")));
    crate::test_budget::await_spawned(
        Duration::from_secs(5),
        "the fake server to receive initialize",
        || params.is_file(),
    );
    runtime.handle().clone().block_on(state.shutdown_all());
    let json = std::fs::read_to_string(&params).expect("initialize params");
    (serde_json::from_str(&json).expect("json"), root)
}

/// #1517: `initialize` sent `rootUri: null` beside `workspaceFolders`.
/// `rootUri` is deprecated but still required, and null means "no folder
/// is open": lua-language-server then never loads the workspace and
/// answers nothing. It now names the workspace, as VS Code and Neovim
/// do, with `rootPath` for servers older still.
#[test]
fn initialize_names_the_workspace_as_root_uri_and_root_path() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let (params, root) = initialize_params_for_workspace("lp");
    let folder = &params["workspaceFolders"][0]["uri"];
    assert!(
        folder.is_string(),
        "workspaceFolders is still sent: {params}"
    );
    assert_eq!(
        &params["rootUri"], folder,
        "rootUri is the workspace folder"
    );
    assert_eq!(params["rootPath"], root.display().to_string());
}

/// Negative: a workspace path that needs escaping gives `rootUri` the
/// same percent-encoded URI as its workspace folder, and `rootPath`
/// the plain path, not the URI.
#[test]
fn root_uri_matches_the_workspace_folder_for_a_path_with_spaces() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let (params, root) = initialize_params_for_workspace("my lua project");
    let uri = params["rootUri"].as_str().expect("rootUri is set");
    assert!(
        uri.starts_with("file:///") && uri.ends_with("my%20lua%20project"),
        "{uri}"
    );
    assert_eq!(&params["rootUri"], &params["workspaceFolders"][0]["uri"]);
    assert_eq!(params["rootPath"], root.display().to_string());
}

/// Open, save and close one document against [`FAKE_LSP_RECORDER`]
/// advertising `capabilities` (a Python literal), and return the method
/// log once the close has arrived (#854).
///
/// The close is the barrier that makes an ABSENT didSave observable: the
/// notifications travel one ordered stream, so a server that has seen
/// didClose has seen every didSave that was sent before it.
fn saves_seen_by(capabilities: &str) -> String {
    saves_seen_by_each(&[capabilities]).remove(0)
}

/// [`saves_seen_by`] for several servers on the one document, each a
/// [`FAKE_LSP_RECORDER`] advertising its own `capabilities`, in order;
/// one method log per server.
fn saves_seen_by_each(capabilities: &[&str]) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "x = 1\n").expect("write demo");
    let mut registry = ServerRegistry::new();
    let mut logs = Vec::new();
    for (i, caps) in capabilities.iter().enumerate() {
        let script = root.join(format!("fake_lsp_{i}.py"));
        std::fs::write(
            &script,
            format!("CAPABILITIES = {caps}\n{FAKE_LSP_RECORDER}"),
        )
        .expect("write fake server");
        let log = root.join(format!("methods_{i}.log"));
        registry.register(
            Language::PYTHON,
            ServerConfig {
                // Named apart, so no server stands in for another.
                name: Box::leak(format!("fake-recorder-{i}").into_boxed_str()),
                command: "python3".into(),
                args: vec![script.display().to_string(), log.display().to_string()],
                language: Language::PYTHON,
                initialization_options: None,
                provision: None,
            },
        );
        logs.push(log);
    }
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };

    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state.open_doc(file.clone(), String::from("x = 1\n")).await;
        state.save_doc(file.clone(), String::from("x = 2\n")).await;
        state.close_doc(file.clone()).await;
    });

    // Up to three python3 servers start, initialize and see the document
    // open, save and close: a base as wide as the other spawn waits,
    // which costs nothing once every log shows didClose.
    crate::test_budget::await_spawned(
        Duration::from_secs(5),
        "every fake server to receive didClose",
        || {
            logs.iter().all(|log| {
                std::fs::read_to_string(log)
                    .unwrap_or_default()
                    .contains("textDocument/didClose")
            })
        },
    );
    runtime.handle().clone().block_on(state.shutdown_all());
    logs.iter()
        .map(|log| {
            let methods = std::fs::read_to_string(log).unwrap_or_default();
            assert!(
                methods.contains("textDocument/didOpen"),
                "sanity: the fake server must have received didOpen; got: {methods:?}"
            );
            methods
        })
        .collect()
}

/// Issue #37: rust-analyzer re-runs its check-on-save (`cargo check`, the
/// source of the Rust PROBLEMS entries) only when the client sends
/// `textDocument/didSave`. Saving a file must therefore emit didSave, or
/// the PROBLEMS panel goes permanently stale after the first open.
///
/// The server advertises what rust-analyzer does when the client declares
/// no dynamic registration (#854): `save: {}`, which asks for the
/// notification without the text.
#[test]
fn save_doc_sends_did_save_notification_to_the_server() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let methods =
        saves_seen_by(r#"{"textDocumentSync": {"openClose": True, "change": 2, "save": {}}}"#);
    assert!(
        methods.lines().any(|l| l == "textDocument/didSave null"),
        "saving a document must send textDocument/didSave, without the text \
             the server did not ask for; server received: {methods:?}"
    );
}

/// #854: a server whose `textDocumentSync` omits `save` asked not to be
/// told about saves ("if omitted the notification should not be sent").
/// This is ruff 0.15's exact capability, and it logs "Received
/// notification textDocument/didSave which does not have a handler" on
/// every Ctrl+S it is sent anyway.
#[test]
fn save_doc_skips_a_server_that_did_not_ask_for_saves() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let methods = saves_seen_by(
        r#"{"textDocumentSync": {"change": 2, "openClose": True, "willSave": False, "willSaveWaitUntil": False}}"#,
    );
    assert!(
        !methods.contains("textDocument/didSave"),
        "a server that advertised no `save` must not be sent didSave; \
             server received: {methods:?}"
    );
}

/// #854: `save: { includeText: true }` asks for the saved text in the
/// notification, and croft used to send `text: None` to everyone.
#[test]
fn save_doc_sends_the_text_to_a_server_that_asked_for_it() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let methods = saves_seen_by(
        r#"{"textDocumentSync": {"openClose": True, "change": 1, "save": {"includeText": True}}}"#,
    );
    assert!(
        methods
            .lines()
            .any(|l| l == r#"textDocument/didSave "x = 2\n""#),
        "includeText must put the saved buffer in didSave; server received: {methods:?}"
    );
}

/// #854 guard: servers sharing a document are each answered by their
/// own capability, from the one save: the one that asked for the text
/// gets it, the one that asked for a bare save gets no text, and the one
/// that asked for nothing gets nothing.
#[test]
fn save_doc_answers_each_server_on_a_document_by_its_own_capability() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let logs = saves_seen_by_each(&[
        r#"{"textDocumentSync": {"openClose": True, "change": 1, "save": {"includeText": True}}}"#,
        r#"{"textDocumentSync": {"openClose": True, "change": 2, "save": {}}}"#,
        r#"{"textDocumentSync": {"openClose": True, "change": 2}}"#,
    ]);
    let saves = |log: &str| -> Vec<String> {
        log.lines()
            .filter(|l| l.starts_with("textDocument/didSave"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(saves(&logs[0]), vec![r#"textDocument/didSave "x = 2\n""#]);
    assert_eq!(saves(&logs[1]), vec!["textDocument/didSave null"]);
    assert!(saves(&logs[2]).is_empty(), "{:?}", logs[2]);
}

/// #854 review guard: the saved text reaches every server that asks for
/// it, not just the last one, which is handed the buffer itself while
/// the ones before it get a copy.
#[test]
fn every_server_that_asks_for_the_text_gets_it() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let logs = saves_seen_by_each(&[
        r#"{"textDocumentSync": {"openClose": True, "change": 1, "save": {"includeText": True}}}"#,
        r#"{"textDocumentSync": {"openClose": True, "change": 2, "save": {}}}"#,
        r#"{"textDocumentSync": {"openClose": True, "change": 1, "save": {"includeText": True}}}"#,
    ]);
    let saves = |log: &str| -> Vec<String> {
        log.lines()
            .filter(|l| l.starts_with("textDocument/didSave"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(saves(&logs[0]), vec![r#"textDocument/didSave "x = 2\n""#]);
    assert_eq!(saves(&logs[1]), vec!["textDocument/didSave null"]);
    assert_eq!(saves(&logs[2]), vec![r#"textDocument/didSave "x = 2\n""#]);
}

/// #854 guard: the fix does not reach a server that picked the older
/// `textDocumentSync: <kind>` number. That form predates `save`, and such
/// a server is still told about saves, without the text, as VS Code,
/// Neovim and Helix tell it.
#[test]
fn save_doc_still_sends_a_bare_save_to_a_server_with_the_old_sync_kind() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let methods = saves_seen_by(r#"{"textDocumentSync": 2}"#);
    assert!(
        methods.lines().any(|l| l == "textDocument/didSave null"),
        "server received: {methods:?}"
    );
}

/// #854 guard: `includeText: false` asks for a bare save, never the text.
#[test]
fn save_doc_sends_no_text_when_include_text_is_false() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let methods = saves_seen_by(
        r#"{"textDocumentSync": {"openClose": True, "change": 2, "save": {"includeText": False}}}"#,
    );
    assert!(
        methods.lines().any(|l| l == "textDocument/didSave null"),
        "server received: {methods:?}"
    );
}

/// #854: every shape `textDocumentSync` can take, read the way the spec
/// (and, for the pre-`save` kind number, VS Code) reads it.
#[test]
fn save_notify_reads_every_text_document_sync_shape() {
    use lsp_types::{
        SaveOptions, TextDocumentSyncCapability as Sync, TextDocumentSyncKind,
        TextDocumentSyncOptions, TextDocumentSyncSaveOptions as Save,
    };
    let options = |save: Option<Save>| {
        Some(Sync::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            save,
            ..Default::default()
        }))
    };
    let with = |include_text| Some(Save::SaveOptions(SaveOptions { include_text }));
    assert_eq!(save_notify_of(&None), SaveNotify::Never);
    assert_eq!(save_notify_of(&options(None)), SaveNotify::Never);
    assert_eq!(
        save_notify_of(&options(Some(Save::Supported(false)))),
        SaveNotify::Never
    );
    assert_eq!(
        save_notify_of(&options(Some(Save::Supported(true)))),
        SaveNotify::WithoutText
    );
    assert_eq!(
        save_notify_of(&options(with(None))),
        SaveNotify::WithoutText
    );
    assert_eq!(
        save_notify_of(&options(with(Some(false)))),
        SaveNotify::WithoutText
    );
    assert_eq!(
        save_notify_of(&options(with(Some(true)))),
        SaveNotify::WithText
    );
    assert_eq!(
        save_notify_of(&Some(Sync::Kind(TextDocumentSyncKind::FULL))),
        SaveNotify::WithoutText
    );
    assert_eq!(
        save_notify_of(&Some(Sync::Kind(TextDocumentSyncKind::NONE))),
        SaveNotify::Never
    );
}

/// #908: servers such as ty register the files they want to hear about
/// only when the client declares it can take the registration.
#[test]
fn client_capabilities_declare_watched_files_registration() {
    let watched = build_client_capabilities()
        .workspace
        .and_then(|w| w.did_change_watched_files)
        .expect("workspace.didChangeWatchedFiles must be declared");
    assert_eq!(watched.dynamic_registration, Some(true));
    assert_eq!(watched.relative_pattern_support, Some(true));
}

/// #854: rust-analyzer advertises `save` only while the client declares
/// no dynamic registration for document sync; told it may register, it
/// drops `save` and sends `client/registerCapability`, which croft
/// declines, and check-on-save would stop. Pin the declaration.
#[test]
fn client_capabilities_declare_did_save_without_dynamic_registration() {
    let caps = build_client_capabilities();
    let sync = caps
        .text_document
        .and_then(|td| td.synchronization)
        .expect("textDocument.synchronization must be declared");
    assert_eq!(sync.did_save, Some(true));
    assert_eq!(
        sync.dynamic_registration,
        Some(false),
        "dynamic registration would move rust-analyzer's didSave to a \
             registerCapability croft does not handle"
    );
}

/// A fake server that advertises `inlayHintProvider` and answers
/// `textDocument/inlayHint` with one plain-string hint (with padding) and
/// one label-parts hint, covering both wire shapes end to end.
const FAKE_LSP_INLAY: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"inlayHintProvider": True}}})
        elif method == "textDocument/inlayHint":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [
                {"position": {"line": 0, "character": 5}, "label": ": int"},
                {"position": {"line": 0, "character": 9},
                 "label": [{"value": "n"}, {"value": ":"}],
                 "paddingRight": True},
            ]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// A server as strict as rust-analyzer about positions (#1490): it
/// remembers each opened document and answers an inlay-hint request whose
/// range ends past the document with `-32603 Invalid offset`.
const FAKE_LSP_STRICT_INLAY: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

docs = {}
while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    params = msg.get("params") or {}
    if method == "textDocument/didOpen":
        doc = params["textDocument"]
        docs[doc["uri"]] = doc["text"]
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"inlayHintProvider": True}}})
        elif method == "textDocument/inlayHint":
            lines = docs.get(params["textDocument"]["uri"], "").split("\n")
            end = params["range"]["end"]
            line, col = end["line"], end["character"]
            in_range = line < len(lines) and col <= len(lines[line].encode("utf-16-le")) // 2
            if in_range:
                send({"jsonrpc": "2.0", "id": msg["id"], "result": [
                    {"position": {"line": 1, "character": 9}, "label": ": u32"},
                ]})
            else:
                send({"jsonrpc": "2.0", "id": msg["id"], "error": {
                    "code": -32603,
                    "message": "Invalid offset LineCol { line: %d, col: %d }" % (line, col)}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// Opens `text` on the strict server and asks for inlay hints ending at
/// `end`; returns the hints that reach the drain channel.
fn strict_inlay_hints(text: &str, end: Position) -> Vec<InlayHintItem> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("main.py");
    std::fs::write(&file, text).expect("write demo");
    let script = root.join("fake_strict_lsp.py");
    std::fs::write(&script, FAKE_LSP_STRICT_INLAY).expect("write fake server");
    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-strict",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state.open_doc(file.clone(), text.to_string()).await;
        state.request_inlay_hints(file.clone(), end, 7, &tx).await;
    });
    let update = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("an inlay-hint reply must reach the drain channel");
    runtime.handle().clone().block_on(state.shutdown_all());
    update.hints
}

#[test]
fn a_strict_server_answers_inlay_hints_for_the_whole_document() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let text = "def main():\n    v = 40 + 2\n    print(v)\n";
    let hints = strict_inlay_hints(text, document_end(text));
    assert_eq!(
        hints,
        vec![InlayHintItem {
            line: 1,
            character: 9,
            label: String::from(": u32"),
        }],
        "a range ending at the document's end must not be rejected"
    );
}

#[test]
fn a_strict_server_answers_inlay_hints_without_a_final_newline() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let text = "def main():\n    v = 40 + 2\n    print(v)";
    assert_eq!(strict_inlay_hints(text, document_end(text)).len(), 1);
}

/// Negative: the fake server really is strict, so the tests above would
/// catch a range one line past the end (what croft used to send).
#[test]
fn a_strict_server_rejects_a_range_one_line_past_the_end() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let text = "def main():\n    v = 40 + 2\n    print(v)\n";
    let end = document_end(text);
    assert!(strict_inlay_hints(text, Position::new(end.line + 1, 0)).is_empty());
}

#[test]
fn document_end_is_the_last_line_and_its_utf16_length() {
    assert_eq!(document_end(""), Position::new(0, 0));
    assert_eq!(document_end("x = 1\n"), Position::new(1, 0));
    assert_eq!(document_end("a\nbc"), Position::new(1, 2));
    assert_eq!(document_end("a\n\n"), Position::new(2, 0));
    // UTF-16 units, not bytes or chars: `é` is one, `😀` is two.
    assert_eq!(document_end("x\né😀"), Position::new(1, 3));
}

#[test]
fn rows_end_stops_at_the_document_end() {
    let text = "a\nb\nc\n";
    // Within the document: the start of the row after the range.
    assert_eq!(rows_end(text, 0), Position::new(0, 0));
    assert_eq!(rows_end(text, 2), Position::new(2, 0));
    assert_eq!(rows_end(text, 3), Position::new(3, 0));
    // A viewport taller than the file ends where the file does.
    assert_eq!(rows_end(text, 19), Position::new(3, 0));
    assert_eq!(rows_end("a\nbc", 19), Position::new(1, 2));
}

/// The full worker wire path: capability gate, `textDocument/inlayHint`
/// request, label normalisation, and the seq-tagged reply on the drain
/// channel.
#[test]
fn request_inlay_hints_round_trips_through_a_hinting_server() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "x = f(1)\n").expect("write demo");
    let script = root.join("fake_inlay_lsp.py");
    std::fs::write(&script, FAKE_LSP_INLAY).expect("write fake server");

    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-inlay",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };

    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .open_doc(file.clone(), String::from("x = f(1)\n"))
            .await;
        state
            .request_inlay_hints(file.clone(), document_end("x = f(1)\n"), 42, &tx)
            .await;
    });

    let update = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("an inlay-hint reply must reach the drain channel");
    assert_eq!(update.path, file);
    assert_eq!(update.seq, 42, "the reply must echo the request's seq");
    assert_eq!(
        update.hints,
        vec![
            InlayHintItem {
                line: 0,
                character: 5,
                label: String::from(": int"),
            },
            InlayHintItem {
                line: 0,
                character: 9,
                label: String::from("n: "),
            },
        ],
        "both label shapes must normalise, padding folded in"
    );
    runtime.handle().clone().block_on(state.shutdown_all());
}

#[test]
fn occurrence_items_map_ranges_and_the_write_kind() {
    use lsp_types::{DocumentHighlight, DocumentHighlightKind, Position, Range};
    let hs = vec![
        DocumentHighlight {
            range: Range {
                start: Position {
                    line: 2,
                    character: 4,
                },
                end: Position {
                    line: 2,
                    character: 9,
                },
            },
            kind: Some(DocumentHighlightKind::WRITE),
        },
        DocumentHighlight {
            range: Range {
                start: Position {
                    line: 5,
                    character: 0,
                },
                end: Position {
                    line: 5,
                    character: 5,
                },
            },
            kind: None,
        },
    ];
    assert_eq!(
        occurrence_items(hs),
        vec![
            OccurrenceItem {
                start_line: 2,
                start_char: 4,
                end_line: 2,
                end_char: 9,
                write: true,
            },
            OccurrenceItem {
                start_line: 5,
                start_char: 0,
                end_line: 5,
                end_char: 5,
                write: false,
            },
        ],
        "WRITE maps to write=true; an absent kind defaults to a read tint"
    );
}

/// A fake server whose completion list is marked `isIncomplete` and
/// names the `triggerKind` it was asked with (#1529).
const FAKE_LSP_INCOMPLETE: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {
                "completionProvider": {"triggerCharacters": ["/", "->", "@"]}}}})
        elif method == "textDocument/completion":
            context = msg["params"].get("context", {})
            kind = context.get("triggerKind")
            typed = context.get("triggerCharacter") or ""
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "isIncomplete": True,
                "items": [{"label": "kind%s%s" % (kind, typed)}]}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// #1529: an `isIncomplete` list reaches the app as such, and the
/// re-ask goes out as `TriggerForIncompleteCompletions` (3); a first
/// ask stays `Invoked` (1). #929: a trigger character goes as one (2),
/// and the server's one-character `triggerCharacters` are recorded for
/// its own project root, which another root's probe does not touch.
#[test]
fn an_incomplete_completion_list_is_reported_and_re_asked_with_kind_3() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "lo\n").expect("write demo");
    let script = root.join("fake_incomplete_lsp.py");
    std::fs::write(&script, FAKE_LSP_INCOMPLETE).expect("write fake server");
    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-incomplete",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };
    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state.open_doc(file.clone(), String::from("lo\n")).await;
        state
            .request_completion(1, file.clone(), 0, 2, CompletionTrigger::Invoked, &tx)
            .await;
    });
    let first = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("first reply");
    runtime.handle().clone().block_on(async {
        state
            .request_completion(2, file.clone(), 0, 2, CompletionTrigger::Incomplete, &tx)
            .await;
    });
    let again = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("second reply");
    runtime.handle().clone().block_on(async {
        let typed = CompletionTrigger::Character('/');
        state
            .request_completion(3, file.clone(), 0, 2, typed, &tx)
            .await;
    });
    let typed = rx.recv_timeout(Duration::from_secs(30)).expect("reply");
    assert_eq!(typed.items[0].label, "kind2/", "a trigger character");
    let support = state.capability_support.lock().unwrap();
    let doc_root = support.document_roots[&file].clone();
    let triggers = support
        .completion_triggers
        .get(&(Language::PYTHON, doc_root.clone()));
    assert_eq!(triggers.map(String::as_str), Some("/@"), "not `->`");
    drop(support);
    assert!(first.is_incomplete, "isIncomplete reaches the app");
    assert_eq!(first.items[0].label, "kind1", "a first ask is Invoked");
    assert_eq!(again.request_id, 2);
    assert_eq!(
        again.items[0].label, "kind3",
        "the re-ask is TriggerForIncompleteCompletions"
    );
    // Another project root of the language, whose probe finds no server
    // that completes, leaves this root's triggers as they were.
    state.registry = ServerRegistry::new();
    let other = root.join("other");
    runtime
        .handle()
        .clone()
        .block_on(state.ensure_clients(Language::PYTHON, &other));
    let support = state.capability_support.lock().unwrap();
    let kept = support
        .completion_triggers
        .get(&(Language::PYTHON, doc_root));
    assert_eq!(
        kept.map(String::as_str),
        Some("/@"),
        "kept for its own root"
    );
    drop(support);
    runtime.handle().clone().block_on(state.shutdown_all());
}

/// A fake server that advertises `documentHighlightProvider` and answers
/// `textDocument/documentHighlight` with one write and one kindless
/// occurrence, covering the wire shapes end to end.
const FAKE_LSP_HIGHLIGHT: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"documentHighlightProvider": True}}})
        elif method == "textDocument/documentHighlight":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [
                {"range": {"start": {"line": 0, "character": 0},
                           "end": {"line": 0, "character": 1}},
                 "kind": 3},
                {"range": {"start": {"line": 0, "character": 4},
                           "end": {"line": 0, "character": 5}}},
            ]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// The full worker wire path for occurrences: capability gate,
/// `textDocument/documentHighlight` request, kind normalisation, and the
/// request-id-tagged reply on the drain channel.
#[test]
fn request_document_highlights_round_trips_through_a_highlighting_server() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "x = 1; x\n").expect("write demo");
    let script = root.join("fake_highlight_lsp.py");
    std::fs::write(&script, FAKE_LSP_HIGHLIGHT).expect("write fake server");

    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-highlight",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };

    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .open_doc(file.clone(), String::from("x = 1; x\n"))
            .await;
        state
            .request_document_highlights(7, file.clone(), 0, 0, &tx)
            .await;
    });

    let result = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("a document-highlight reply must reach the drain channel");
    assert_eq!(result.request_id, 7, "the reply must echo the request id");
    assert_eq!(
        result.items,
        vec![
            OccurrenceItem {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
                write: true,
            },
            OccurrenceItem {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 5,
                write: false,
            },
        ]
    );
    runtime.handle().clone().block_on(state.shutdown_all());
}

#[test]
fn incoming_call_sites_prefer_the_call_range_over_the_definition() {
    use lsp_types::{CallHierarchyIncomingCall, CallHierarchyItem, Position, Range, SymbolKind};
    let item = |name: &str, line: u32| CallHierarchyItem {
        name: name.to_string(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: Url::from_file_path("/proj/src/lib.rs").unwrap(),
        range: Range {
            start: Position { line, character: 0 },
            end: Position {
                line: line + 5,
                character: 0,
            },
        },
        selection_range: Range {
            start: Position { line, character: 3 },
            end: Position { line, character: 8 },
        },
        data: None,
    };
    let calls = vec![
        CallHierarchyIncomingCall {
            from: item("caller_a", 10),
            from_ranges: vec![Range {
                start: Position {
                    line: 12,
                    character: 8,
                },
                end: Position {
                    line: 12,
                    character: 14,
                },
            }],
        },
        CallHierarchyIncomingCall {
            from: item("caller_b", 30),
            from_ranges: Vec::new(),
        },
    ];
    let sites = incoming_call_sites(calls);
    assert_eq!(sites.len(), 2);
    assert_eq!(sites[0].name, "caller_a");
    assert_eq!(
        (sites[0].line, sites[0].character),
        (12, 8),
        "jump to the CALL expression when the server reports it"
    );
    assert_eq!(
        (sites[1].line, sites[1].character),
        (30, 3),
        "fall back to the caller's own selection range"
    );
}

#[test]
fn outgoing_call_sites_target_the_callee_definition() {
    use lsp_types::{CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range, SymbolKind};
    let callee = CallHierarchyItem {
        name: String::from("helper"),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: Url::from_file_path("/proj/src/util.rs").unwrap(),
        range: Range {
            start: Position {
                line: 4,
                character: 0,
            },
            end: Position {
                line: 9,
                character: 0,
            },
        },
        selection_range: Range {
            start: Position {
                line: 4,
                character: 7,
            },
            end: Position {
                line: 4,
                character: 13,
            },
        },
        data: None,
    };
    let sites = outgoing_call_sites(vec![CallHierarchyOutgoingCall {
        to: callee,
        from_ranges: Vec::new(),
    }]);
    assert_eq!(sites[0].name, "helper");
    assert!(sites[0].path.ends_with("src/util.rs"));
    assert_eq!(
        (sites[0].line, sites[0].character),
        (4, 7),
        "outgoing jumps land on the callee's definition"
    );
}

/// A fake server that advertises `callHierarchyProvider`, answers
/// `textDocument/prepareCallHierarchy` with one item and
/// `callHierarchy/incomingCalls` with one caller.
const FAKE_LSP_CALLS: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

ITEM = None
while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"callHierarchyProvider": True}}})
        elif method == "textDocument/prepareCallHierarchy":
            uri = msg["params"]["textDocument"]["uri"]
            ITEM = {"name": "target_fn", "kind": 12, "uri": uri,
                    "range": {"start": {"line": 0, "character": 0},
                              "end": {"line": 2, "character": 0}},
                    "selectionRange": {"start": {"line": 0, "character": 4},
                                       "end": {"line": 0, "character": 12}}}
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [ITEM]})
        elif method == "callHierarchy/incomingCalls":
            caller = dict(ITEM)
            caller["name"] = "the_caller"
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [
                {"from": caller,
                 "fromRanges": [{"start": {"line": 7, "character": 4},
                                 "end": {"line": 7, "character": 12}}]},
            ]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// The full worker wire path for call hierarchy: capability gate, the
/// prepare + incomingCalls two-step, and the request-id-tagged reply.
#[test]
fn request_call_hierarchy_round_trips_through_a_supporting_server() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "def target_fn():\n    pass\n").expect("write demo");
    let script = root.join("fake_calls_lsp.py");
    std::fs::write(&script, FAKE_LSP_CALLS).expect("write fake server");

    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-calls",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };

    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .open_doc(file.clone(), String::from("def target_fn():\n    pass\n"))
            .await;
        state
            .request_call_hierarchy(9, file.clone(), 0, 4, true, &tx)
            .await;
    });

    let result = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("a call-hierarchy reply must reach the drain channel");
    assert_eq!(result.request_id, 9);
    assert_eq!(result.kind, HierarchyKind::IncomingCalls);
    assert!(!result.unsupported);
    assert_eq!(result.sites.len(), 1);
    assert_eq!(result.sites[0].name, "the_caller");
    assert_eq!(
        (result.sites[0].line, result.sites[0].character),
        (7, 4),
        "the site is the call expression from fromRanges"
    );
    runtime.handle().clone().block_on(state.shutdown_all());
}

/// A fake server that advertises `typeHierarchyProvider` (a key
/// `lsp-types` 0.95 drops), answering prepare with one item and
/// `typeHierarchy/supertypes` with one parent.
const FAKE_LSP_TYPES: &str = r#"
import json, sys

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def item(name, uri, line):
    r = {"start": {"line": line, "character": 6}, "end": {"line": line, "character": 6 + len(name)}}
    return {"name": name, "kind": 5, "uri": uri, "range": r, "selectionRange": r}

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method", "")
    if "id" in msg:
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"capabilities": {"typeHierarchyProvider": True}}})
        elif method == "textDocument/prepareTypeHierarchy":
            uri = msg["params"]["textDocument"]["uri"]
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [item("Child", uri, 0)]})
        elif method == "typeHierarchy/supertypes":
            uri = msg["params"]["item"]["uri"]
            send({"jsonrpc": "2.0", "id": msg["id"], "result": [item("Base", uri, 3)]})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    if method == "exit":
        break
"#;

/// The worker wire path for type hierarchy (#613): the capability read
/// from the raw initialize result, prepare + supertypes, and the reply.
#[test]
fn request_type_hierarchy_round_trips_through_a_supporting_server() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize");
    let file = root.join("demo.py");
    std::fs::write(&file, "class Child(Base):\n    pass\n").expect("write demo");
    let script = root.join("fake_types_lsp.py");
    std::fs::write(&script, FAKE_LSP_TYPES).expect("write fake server");

    let mut registry = ServerRegistry::new();
    registry.register(
        Language::PYTHON,
        ServerConfig {
            name: "fake-types",
            command: "python3".into(),
            args: vec![script.display().to_string()],
            language: Language::PYTHON,
            initialization_options: None,
            provision: None,
        },
    );
    let (diag_tx, _diag_rx) = std_mpsc::channel();
    let (prog_tx, _prog_rx) = std_mpsc::channel();
    let mut state = WorkerState {
        workspace_root: root.clone(),
        extra_roots: Vec::new(),
        registry,
        clients: HashMap::new(),
        docs: HashMap::new(),
        capability_support: Arc::new(StdMutex::new(LangCapabilitySupport::default())),
        semantic_refresh: Arc::new(AtomicBool::new(false)),
        inlay_refresh: Arc::new(AtomicBool::new(false)),
        diagnostic_refresh: Arc::new(AtomicBool::new(false)),
        diagnostics_tx: diag_tx,
        progress_tx: prog_tx,
        restarts: ServerRestarts::default(),
    };

    let (tx, rx) = std_mpsc::channel();
    let runtime = LspRuntime::new().expect("runtime");
    runtime.handle().clone().block_on(async {
        state
            .open_doc(file.clone(), String::from("class Child(Base):\n    pass\n"))
            .await;
        state
            .request_type_hierarchy(9, file.clone(), 0, 6, true, &tx)
            .await;
    });

    let result = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("a type-hierarchy reply must reach the drain channel");
    assert_eq!(result.request_id, 9);
    assert_eq!(result.kind, HierarchyKind::Supertypes);
    assert!(!result.unsupported);
    assert_eq!(result.sites.len(), 1);
    assert_eq!(result.sites[0].name, "Base");
    assert_eq!(
        (result.sites[0].line, result.sites[0].character),
        (3, 6),
        "the site is the supertype's name, from selectionRange"
    );
    runtime.handle().clone().block_on(state.shutdown_all());
}

/// A nested `documentSymbol` reply from JSON.
fn nested_reply(json: serde_json::Value) -> DocumentSymbolResponse {
    DocumentSymbolResponse::Nested(serde_json::from_value(json).unwrap())
}

fn doc_symbol(name: &str, line: u32, children: serde_json::Value) -> serde_json::Value {
    let pos = serde_json::json!({"line": line, "character": 2});
    let range = serde_json::json!({"start": pos, "end": pos});
    serde_json::json!({
        "name": name, "kind": 5, "range": range, "selectionRange": range,
        "children": children,
    })
}

/// #1220: tsserver sorts each level of its navigation tree by name, so
/// OUTLINE and Go to Symbol listed `Cart` before `sum` and `cents`
/// before `id`. Every level comes out in file order, as in VS Code.
#[test]
fn nested_symbols_list_in_file_order_at_every_level() {
    let reply = nested_reply(serde_json::json!([
        doc_symbol(
            "Cart",
            11,
            serde_json::json!([
                doc_symbol("add", 13, serde_json::json!([])),
                doc_symbol("items", 12, serde_json::json!([])),
                doc_symbol("total", 16, serde_json::json!([])),
            ])
        ),
        doc_symbol("handler", 6, serde_json::json!([])),
        doc_symbol(
            "Item",
            0,
            serde_json::json!([
                doc_symbol("cents", 0, serde_json::json!([])),
                doc_symbol("id", 0, serde_json::json!([])),
            ])
        ),
        doc_symbol("sum", 2, serde_json::json!([])),
    ]));
    let mut reply = reply;
    // `cents` is declared after `id` on the same line.
    if let DocumentSymbolResponse::Nested(syms) = &mut reply {
        let item = syms.iter_mut().find(|s| s.name == "Item").unwrap();
        let fields = item.children.as_mut().unwrap();
        fields[0].selection_range.start.character = 30;
        fields[1].selection_range.start.character = 17;
    }
    let names: Vec<(String, u16)> = flatten_symbols(reply)
        .into_iter()
        .map(|s| (s.name, s.depth))
        .collect();
    let expected = [
        ("Item", 0),
        ("id", 1),
        ("cents", 1),
        ("sum", 0),
        ("handler", 0),
        ("Cart", 0),
        ("items", 1),
        ("add", 1),
        ("total", 1),
    ];
    let expected: Vec<(String, u16)> = expected.iter().map(|(n, d)| (n.to_string(), *d)).collect();
    assert_eq!(names, expected);
}

/// Negative (#1220): a reply already in file order (Python, Rust) comes
/// out unchanged, children stay under their own parent, and the flat
/// `SymbolInformation` shape still sorts by position.
#[test]
fn symbols_already_in_file_order_and_flat_replies_are_unchanged() {
    let reply = nested_reply(serde_json::json!([
        doc_symbol(
            "first",
            1,
            serde_json::json!([doc_symbol("late_child", 9, serde_json::json!([])),])
        ),
        doc_symbol("second", 4, serde_json::json!([])),
    ]));
    let names: Vec<(String, u16)> = flatten_symbols(reply)
        .into_iter()
        .map(|s| (s.name, s.depth))
        .collect();
    assert_eq!(
        names,
        vec![
            ("first".to_string(), 0),
            ("late_child".to_string(), 1),
            ("second".to_string(), 0)
        ],
        "a child declared after the next sibling's line stays under its parent"
    );
    let pos = |line: u32| {
        serde_json::json!({"start": {"line": line, "character": 0},
                                             "end": {"line": line, "character": 1}})
    };
    let flat: Vec<lsp_types::SymbolInformation> = serde_json::from_value(serde_json::json!([
        {"name": "b", "kind": 12, "location": {"uri": "file:///x.py", "range": pos(5)}},
        {"name": "a", "kind": 12, "location": {"uri": "file:///x.py", "range": pos(2)}},
    ]))
    .unwrap();
    let names: Vec<String> = flatten_symbols(DocumentSymbolResponse::Flat(flat))
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["a", "b"]);
}
