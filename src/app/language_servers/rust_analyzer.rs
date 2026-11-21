//! rust-analyzer's own `Rust:` commands: whether it serves the active file, sending one for
//! the caret, and acting on the answer (a macro's expansion opened as a tab, a jump to the
//! parent module or Cargo.toml, a proc-macro rebuild or workspace reload reported).

use super::*;

impl App {
    /// Whether rust-analyzer is open on the active tab's file (#1626), so
    /// its `Rust:` commands have a server to go to. Asked of the servers
    /// open on the file, not those reporting diagnostics on it: one slow
    /// diagnostic pull retires rust-analyzer from reporting while its
    /// client keeps answering.
    pub(in crate::app) fn rust_analyzer_serves_active_file(&self) -> bool {
        match (self.editor.path.as_deref(), self.lsp.as_ref()) {
            (Some(path), Some(lsp)) => {
                lsp.document_served_by(path, crate::lsp::rust_analyzer::SERVER)
            }
            _ => false,
        }
    }

    /// Send a `Rust:` command's request (#1626) for the caret in the
    /// active file; [`App::drain_lsp_rust_analyzer`] acts on the answer.
    pub(in crate::app) fn request_rust_analyzer(
        &mut self,
        request: crate::lsp::rust_analyzer::RaRequest,
    ) {
        use crate::lsp::rust_analyzer::RaRequest;
        if !self.rust_analyzer_serves_active_file() {
            self.status = String::from("rust-analyzer is not running for this file");
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_rust_analyzer(path, request, line, character);
        self.rust_analyzer_request = Some((id, request));
        self.status = String::from(match request {
            RaRequest::ExpandMacro => "Expanding the macro at the caret\u{2026}",
            RaRequest::ParentModule => "Locating the parent module\u{2026}",
            RaRequest::OpenCargoToml => "Finding Cargo.toml\u{2026}",
            RaRequest::RebuildProcMacros => "Asking rust-analyzer to rebuild proc macros\u{2026}",
            RaRequest::ReloadWorkspace => "Asking rust-analyzer to reload the workspace\u{2026}",
        });
    }

    pub fn drain_lsp_rust_analyzer(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_rust_analyzer() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            changed |= self.apply_rust_analyzer_result(result);
        }
        changed
    }

    /// Act on a rust-analyzer answer (#1626) when it is the one awaited:
    /// open an expansion as a Rust tab, jump to a parent module or manifest
    /// (a picker when there are several), or report a workspace request.
    pub(in crate::app) fn apply_rust_analyzer_result(
        &mut self,
        result: crate::lsp::manager::RustAnalyzerResult,
    ) -> bool {
        use crate::lsp::rust_analyzer::{self as ra, RaRequest};
        if self.rust_analyzer_request != Some((result.request_id, result.request)) {
            return false;
        }
        self.rust_analyzer_request = None;
        let value = match result.outcome {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("rust-analyzer: {e}");
                return true;
            }
        };
        match result.request {
            RaRequest::ExpandMacro => match ra::expansion(&value) {
                Some((name, text)) => {
                    let label = self.scratch_buffer_path(&format!("Expansion of {name}!"));
                    match self.editor.open_text_buffer(&label, &text) {
                        Ok(()) => {
                            self.editor
                                .set_language(Some(crate::highlight::LangKind::Rust));
                            self.focus_pane(Pane::Editor);
                            self.status = format!("Expanded {name}!");
                        }
                        Err(e) => self.status = format!("Could not open the expansion: {e}"),
                    }
                }
                None => self.status = String::from("No macro call at the caret"),
            },
            RaRequest::ParentModule | RaRequest::OpenCargoToml => {
                let targets = ra::locations(&value);
                let what = if result.request == RaRequest::ParentModule {
                    "parent modules"
                } else {
                    "Cargo.toml files"
                };
                match targets.len() {
                    0 => self.status = format!("No {what} found"),
                    1 => {
                        let (path, line, col) = targets.into_iter().next().expect("len == 1");
                        self.go_to_definition(path, line, col);
                    }
                    _ => self.open_location_picker(targets, what),
                }
            }
            RaRequest::RebuildProcMacros => {
                self.status =
                    String::from("rust-analyzer is rebuilding proc macros and build scripts")
            }
            RaRequest::ReloadWorkspace => {
                self.status = String::from("rust-analyzer is reloading the workspace")
            }
        }
        true
    }
}
