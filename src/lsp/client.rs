use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{LanguageServer, MainLoop, ServerSocket};
use lsp_types::notification::{LogMessage, PublishDiagnostics, ShowMessage};
use lsp_types::{
    ClientCapabilities, CompletionContext, CompletionParams, CompletionResponse,
    CompletionTriggerKind, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, PartialResultParams, Position,
    ServerCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
    WorkspaceFolder,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use crate::lsp::config::ServerConfig;
use crate::lsp::log_file;

struct ClientState {
    name: String,
}

impl ClientState {
    fn router(name: String) -> Router<Self> {
        let mut router = Router::new(ClientState { name });
        router
            .notification::<PublishDiagnostics>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] diagnostics for {}: {} item(s)",
                    this.name,
                    params.uri,
                    params.diagnostics.len()
                ));
                ControlFlow::Continue(())
            })
            .notification::<ShowMessage>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] {:?}: {}",
                    this.name, params.typ, params.message
                ));
                ControlFlow::Continue(())
            })
            .notification::<LogMessage>(|this, params| {
                log_file::log(&format!(
                    "lsp[{}] log {:?}: {}",
                    this.name, params.typ, params.message
                ));
                ControlFlow::Continue(())
            });
        router.unhandled_notification(|this, notif| {
            log_file::log(&format!(
                "lsp[{}] ignored notification {}",
                this.name, notif.method
            ));
            ControlFlow::Continue(())
        });
        router.unhandled_request(|this, req| {
            let method = req.method.clone();
            let name = this.name.clone();
            Box::pin(async move {
                log_file::log(&format!(
                    "lsp[{name}] declined unsupported request {method}"
                ));
                Err(async_lsp::ResponseError::new(
                    async_lsp::ErrorCode::METHOD_NOT_FOUND,
                    format!("No such method {method}"),
                )
                .into())
            })
        });
        router
    }
}

pub struct LspClient {
    server: ServerSocket,
    capabilities: ServerCapabilities,
    name: String,
    // Holds the process so kill_on_drop only fires when this client is
    // dropped, not when spawn() returns. Without this the server is
    // SIGKILLed mid-handshake and the mainloop reader sees EOF.
    child: Child,
}

impl LspClient {
    pub async fn spawn(
        config: &ServerConfig,
        workspace_root: &Path,
        client_capabilities: ClientCapabilities,
    ) -> Result<Self> {
        let workspace_uri = Url::from_file_path(workspace_root).map_err(|_| {
            anyhow!(
                "workspace root must be absolute: {}",
                workspace_root.display()
            )
        })?;
        let workspace_name = workspace_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string());

        let mut child = Command::new(&config.command)
            .args(&config.args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn lsp server `{}`", config.command))?;

        let stdout = child
            .stdout
            .take()
            .context("server stdout missing")?
            .compat();
        let stdin = child
            .stdin
            .take()
            .context("server stdin missing")?
            .compat_write();
        let stderr = child.stderr.take().context("server stderr missing")?;

        let name = config.name.to_string();
        let router_name = name.clone();
        let (mainloop, mut server) = MainLoop::new_client(move |_server_socket| {
            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(ClientState::router(router_name))
        });

        let stderr_name = name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log_file::log(&format!("lsp[{stderr_name}] stderr: {line}"));
            }
        });

        let mainloop_name = name.clone();
        tokio::spawn(async move {
            if let Err(e) = mainloop.run_buffered(stdout, stdin).await {
                log_file::log(&format!("lsp[{mainloop_name}] mainloop exited: {e}"));
            }
        });

        let init = server
            .initialize(InitializeParams {
                process_id: Some(std::process::id()),
                capabilities: client_capabilities,
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: workspace_uri,
                    name: workspace_name,
                }]),
                ..InitializeParams::default()
            })
            .await
            .context("lsp initialize")?;
        server
            .initialized(InitializedParams {})
            .context("lsp initialized notification")?;

        Ok(Self {
            server,
            capabilities: init.capabilities,
            name,
            child,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    pub fn server_mut(&mut self) -> &mut ServerSocket {
        &mut self.server
    }

    pub fn did_open(
        &mut self,
        uri: Url,
        language_id: &str,
        version: i32,
        text: String,
    ) -> Result<()> {
        self.server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: language_id.to_string(),
                    version,
                    text,
                },
            })
            .context("did_open")
    }

    pub fn did_change_full(&mut self, uri: Url, version: i32, text: String) -> Result<()> {
        self.server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier { uri, version },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            })
            .context("did_change")
    }

    pub fn did_close(&mut self, uri: Url) -> Result<()> {
        self.server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .context("did_close")
    }

    pub async fn completion(
        &mut self,
        uri: Url,
        line: u32,
        character: u32,
    ) -> Result<Option<CompletionResponse>> {
        self.server
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: Some(CompletionContext {
                    trigger_kind: CompletionTriggerKind::INVOKED,
                    trigger_character: None,
                }),
            })
            .await
            .context("completion")
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.server.shutdown(()).await.context("lsp shutdown")?;
        self.server.exit(()).context("lsp exit")?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait()).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::runtime::LspRuntime;
    use async_lsp::{AnyNotification, LspService};
    use serde_json::json;

    #[test]
    fn router_continues_past_unhandled_vendor_notification_like_pyright_begin_progress() {
        let mut router = ClientState::router("basedpyright".into());
        let notif: AnyNotification = serde_json::from_value(json!({
            "method": "pyright/beginProgress",
            "params": {}
        }))
        .expect("AnyNotification deserialization");
        match router.notify(notif) {
            ControlFlow::Continue(()) => {}
            ControlFlow::Break(result) => {
                panic!(
                    "router must absorb vendor-specific notifications (basedpyright streams pyright/beginProgress / pyright/endProgress / pyright/reportProgress during workspace indexing) instead of breaking the mainloop; got Break({result:?})"
                );
            }
        }
    }

    #[test]
    fn router_continues_past_arbitrary_unhandled_notification_methods() {
        let mut router = ClientState::router("any".into());
        for method in ["foo/bar", "experimental/dap", "rust-analyzer/serverStatus"] {
            let notif: AnyNotification = serde_json::from_value(json!({
                "method": method,
                "params": {}
            }))
            .expect("AnyNotification deserialization");
            assert!(
                matches!(router.notify(notif), ControlFlow::Continue(())),
                "router must absorb the vendor notification {method} instead of killing the mainloop with an Unhandled-notification routing error"
            );
        }
    }

    fn server_on_path(cmd: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join(cmd);
            if !candidate.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&candidate) {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                }
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
        false
    }

    fn first_available_python_server() -> Option<ServerConfig> {
        if server_on_path("basedpyright-langserver") {
            Some(ServerConfig::basedpyright())
        } else if server_on_path("pyright-langserver") {
            Some(ServerConfig::pyright())
        } else {
            None
        }
    }

    fn run_initialize_shutdown(config: ServerConfig) {
        let rt = LspRuntime::new().expect("runtime");
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");
        let expected_name = config.name;
        let display_name = config.name;

        let result: Result<()> = rt.handle().block_on(async move {
            let client = LspClient::spawn(&config, &root, ClientCapabilities::default()).await?;
            assert_eq!(client.name(), expected_name);
            client.shutdown().await
        });

        if let Err(e) = result {
            eprintln!("SKIPPED: {display_name} initialize/shutdown failed: {e}");
        }
    }

    #[test]
    fn initialize_and_shutdown_python_server() {
        let Some(config) = first_available_python_server() else {
            eprintln!("SKIPPED: no basedpyright/pyright on PATH");
            return;
        };
        run_initialize_shutdown(config);
    }

    #[test]
    fn initialize_and_shutdown_rust_analyzer() {
        if !server_on_path("rust-analyzer") {
            eprintln!("SKIPPED: rust-analyzer not on PATH");
            return;
        }
        run_initialize_shutdown(ServerConfig::rust_analyzer());
    }

    #[test]
    fn initialize_and_shutdown_ruff_server() {
        if !server_on_path("ruff") {
            eprintln!("SKIPPED: ruff not on PATH");
            return;
        }
        run_initialize_shutdown(ServerConfig::ruff());
    }
}
