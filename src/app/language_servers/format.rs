//! Format Document and Format Selection: the server's reply or Prettier's run applied to
//! the tab it formats, and format-on-save, which writes the file unformatted once its
//! formatter has taken too long.

use super::*;

impl App {
    /// VS Code "Format Document" (Cmd+Opt+Shift+F): ask the language server to
    /// reformat the active buffer. No-op on read-only diff / sheet / image
    /// tabs. The reply is applied in [`drain_lsp_format`].
    pub(in crate::app) fn start_format_document(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        // A Prettier project formats with its own Prettier and config, as
        // VS Code's Prettier extension does; tsserver's formatter knows
        // neither (#1255).
        if let Some(bin) = crate::prettier::for_file(&path) {
            let sent = self.editor.document_text();
            self.prettier_job = Some(crate::prettier::Job::spawn(bin, path, sent));
            self.status = String::from("Formatting document with Prettier");
            return;
        }
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        // Mirror the editor's indentation preference; servers that carry their
        // own config (rustfmt, ruff) ignore these but the fields are
        // required by the LSP request.
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_formatting(path, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = false;
        self.status = String::from("Formatting document");
    }

    /// The `edit_seq` of the tab a format reply for `path` lands in (the
    /// one `apply_rename_to_open_tab` picks), or `None` when no tab holds it.
    pub(in crate::app) fn format_target_seq(&self, path: &std::path::Path) -> Option<u64> {
        let idx = self.editor.find_buffer_tab(path)?;
        Some(self.editor.editors[idx].edit_seq)
    }

    /// "Format Selection" (#254): `textDocument/rangeFormatting` over the
    /// primary selection, applied through the same reply path as
    /// whole-document formatting.
    pub(in crate::app) fn format_selection(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(sel) = self.editor.selection.filter(|s| s.has_area()) else {
            self.status = String::from("Select something to format");
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let ((sr, sc), (er, ec)) = sel.normalised();
        let start = self.editor.pos_to_utf16(sr, sc);
        let end = self.editor.pos_to_utf16(er, ec);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_range_formatting(path, start, end, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = true;
        self.status = String::from("Formatting selection");
    }

    /// Apply a finished Prettier run to the tab it formatted, then let the
    /// save it held go ahead. A buffer edited while Prettier ran keeps the
    /// edit: the result only fits the text that was sent.
    pub fn drain_prettier_format(&mut self) -> bool {
        let Some(reply) = self
            .prettier_job
            .as_ref()
            .and_then(|job| job.poll(std::time::Instant::now()))
        else {
            return false;
        };
        let Some(job) = self.prettier_job.take() else {
            return false;
        };
        let formatted = match reply {
            Ok(formatted) => formatted,
            Err(e) => {
                // Said after the save, which would otherwise cover it with
                // "Saved".
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = match saving {
                    true => format!("Prettier failed: {e}; saved without formatting"),
                    false => format!("Prettier failed: {e}"),
                };
                return true;
            }
        };
        let tab = self.editor.find_buffer_tab(&job.path);
        let buffer = tab
            .map(|i| &self.editor.editors[i])
            .map(|e| (e.document_text(), e.lines.clone(), e.has_final_newline()));
        self.status = match buffer {
            Some((text, lines, final_newline)) if text == job.sent => {
                if formatted == job.sent {
                    String::from("Document already formatted")
                } else {
                    // The buffer keeps no line for a final newline it already
                    // has; one Prettier adds becomes the empty last line that
                    // writes it.
                    let body = match final_newline {
                        true => formatted.strip_suffix('\n').unwrap_or(&formatted),
                        false => &formatted,
                    };
                    let edits = crate::prettier::edits_for(&lines, body);
                    self.editor.apply_rename_to_open_tab(&job.path, &edits);
                    self.editor.clamp_cursor();
                    String::from("Formatted document with Prettier")
                }
            }
            Some(_) => String::from("Prettier result dropped: the file changed while it ran"),
            None => String::from("Format failed: document not open"),
        };
        self.complete_pending_save();
        true
    }

    pub fn drain_lsp_format(&mut self) -> bool {
        self.drain_lsp_format_at(std::time::Instant::now())
    }

    /// Apply a formatter reply that has arrived, or, once a format-on-save
    /// has waited `FORMAT_ON_SAVE_TIMEOUT` with none, write its file
    /// unformatted: a busy, hung or unreachable server left it unsaved for
    /// good (#1491). Returns true when the next frame should redraw.
    pub(in crate::app) fn drain_lsp_format_at(&mut self, now: std::time::Instant) -> bool {
        let reply = match self.lsp.as_ref() {
            Some(lsp) => take_reply(
                &mut self.format_request_id,
                || lsp.drain_formatting(),
                |r| r.request_id,
            ),
            None => None,
        };
        let Some(reply) = reply else {
            let overdue = self.save_after_format.is_some()
                && self
                    .format_save_deadline
                    .is_some_and(|deadline| now >= deadline);
            if overdue {
                let secs = FORMAT_ON_SAVE_TIMEOUT.as_secs();
                self.save_without_formatting(&format!("the formatter did not answer in {secs} s"));
            }
            return overdue;
        };
        self.land_format(
            reply.edits,
            reply.unsupported,
            Some(reply.path),
            reply.server.as_deref(),
        );
        true
    }

    /// Write a format-on-save's file now, unformatted, and drop the reply
    /// it was waiting on, which would otherwise land on the saved text.
    pub(in crate::app) fn save_without_formatting(&mut self, reason: &str) {
        let Some(path) = self.save_after_format.clone() else {
            return;
        };
        self.format_request_id = None;
        self.format_request_seq = None;
        self.format_request_selection = false;
        self.complete_pending_save();
        // A refused write (a disk conflict, a lossy encoding) or a skipped
        // one (the tab was closed meanwhile) keeps the status it set.
        if self.path_is_saved(&path) {
            self.status = format!(
                "Saved {} without formatting: {reason}",
                self.status_path(&path)
            );
        }
    }

    /// Whether `path` is open in some tab, in any split, and none of its
    /// tabs has unsaved edits. A closed file was not written, so it is not
    /// saved either.
    pub(in crate::app) fn path_is_saved(&self, path: &Path) -> bool {
        let mut tabs = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| e.path.as_deref() == Some(path))
            .peekable();
        tabs.peek().is_some() && tabs.all(|e| !e.dirty)
    }

    /// Apply a formatter's reply to the request in flight, then finish a
    /// format-on-save. Split from [`Self::drain_lsp_format`] so tests can
    /// deliver a reply without a server.
    pub(in crate::app) fn land_format(
        &mut self,
        edits: Option<Vec<crate::widgets::editor::TextSpanEdit>>,
        unsupported: bool,
        path: Option<PathBuf>,
        server: Option<&str>,
    ) {
        let selection = std::mem::take(&mut self.format_request_selection);
        let requested_at = self.format_request_seq.take();
        // bash-language-server formats only through shfmt; without it, it
        // answers every request with no edits (#1511), which is not "already
        // formatted". Another formatter's empty reply is its own answer.
        let shfmt_missing = server == Some(BASH_SERVER)
            && path.as_deref().is_some_and(is_shell_script)
            && !tool_on_path("shfmt", self.tool_search_path.as_deref());
        if unsupported {
            self.status = if selection {
                String::from("No range formatter available for this file")
            } else {
                String::from("No formatter available for this file")
            };
            self.complete_pending_save();
            return;
        }
        match (edits, path) {
            // The edits are positions in the text the server was sent; typed
            // into since, they land on the wrong lines (#1127). Drop them, as
            // VS Code does, and let a format-on-save write what is there.
            (Some(edits), Some(path))
                if !edits.is_empty() && self.format_target_seq(&path) != requested_at =>
            {
                let file = self.status_path(&path);
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = if saving {
                    format!(
                        "Saved {file} without formatting: it changed while the formatter was running"
                    )
                } else {
                    format!(
                        "Format skipped: {file} changed while the formatter was running; format again"
                    )
                };
                return;
            }
            (Some(edits), Some(path)) if !edits.is_empty() => {
                match self.editor.apply_rename_to_open_tab(&path, &edits) {
                    Some(_) => {
                        self.editor.clamp_cursor();
                        self.status = if selection {
                            String::from("Formatted selection")
                        } else {
                            String::from("Formatted document")
                        };
                    }
                    None => self.status = String::from("Format failed: document not open"),
                }
            }
            _ if shfmt_missing => self.status = String::from(SHFMT_MISSING),
            _ => {
                self.status = if selection {
                    String::from("Selection already formatted")
                } else {
                    String::from("Document already formatted")
                }
            }
        }
        // A format-on-save write is deferred until now so the disk file gets the
        // formatted text in one go.
        self.complete_pending_save();
    }
}
