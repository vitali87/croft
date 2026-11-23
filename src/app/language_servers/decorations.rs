//! What a server marks in the text besides tokens and hints: document links and following
//! one, folding ranges, color swatches, and Change Color Presentation's picker.

use super::*;

impl App {
    /// Drain document-link batches (#254) into every editor showing the
    /// file — the inlay fan-out, seq gate included. Links change no
    /// pixels, so this never asks for a redraw.
    pub fn drain_lsp_document_links(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_document_links() {
            updates.push(u);
        }
        for u in updates {
            self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_document_links(u.path.clone(), u.links.clone())
            });
        }
    }

    /// Follow a server-resolved document link (#254): `file://` targets
    /// open in the editor, web links go to the system opener — or, on a
    /// relay-capable session, to the user's own browser through the relay.
    ///
    /// Web links only past the `file://` branch, matching
    /// `open_detected_url`: the target comes from a language server and the
    /// text it decorates says nothing about where it points, so a
    /// custom-scheme URI would hand `open`/`xdg-open` an arbitrary target
    /// (app launch, protocol handler) off one disguised Ctrl+click. The
    /// refusal surfaces the real destination in the status line.
    pub(in crate::app) fn open_document_link(&mut self, target: &str) {
        // One normalisation for every branch below, so the string that is
        // checked is the string that is forwarded: surrounding whitespace is
        // dropped, a control character (which the relay's own validator
        // rejects, and which has no business in a URI) refuses outright, and
        // the scheme match is case-insensitive like `file_ref`'s.
        let target = target.trim();
        if target.chars().any(char::is_control) {
            self.status = format!("Refused to open malformed link: {target:?}");
            return;
        }
        let lower = target.to_ascii_lowercase();
        if lower.starts_with("file://") {
            if let Ok(url) = lsp_types::Url::parse(target)
                && let Ok(p) = url.to_file_path()
            {
                // Record where the user was so Back returns to it, exactly
                // like go-to-definition and terminal file links.
                let from = self.editor.path.clone().map(|path| NavLoc {
                    path,
                    row: self.editor.cursor_row,
                    col: self.editor.cursor_col,
                });
                match self.editor.open_pinned(&p) {
                    Ok(()) => {
                        if let Some(loc) = from {
                            self.nav.record(loc);
                        }
                        self.focus_pane(Pane::Editor);
                        self.status = format!("Opened {}", self.status_path(&p));
                    }
                    Err(e) => self.status = format!("Open failed: {e}"),
                }
                return;
            }
            self.status = String::from("Malformed file link");
            return;
        }
        // Deliberately NOT delegated to `open_detected_url`, despite the
        // duplicated guard below: that path first probes `diff://` and
        // `editor_file_uri`, which resolves `vscode://file/<path>` (and the
        // Cursor/Windsurf/Zed schemes) into a file open. A terminal link is
        // text the user's own program printed; a document link's target is
        // chosen by the language server, so letting it name an editor URI
        // would hand a server the ability to open arbitrary paths from a
        // click on unrelated-looking text.
        let scheme_ok = lower.starts_with("http://") || lower.starts_with("https://");
        if !scheme_ok {
            self.status = format!("Refused to open non-web link: {target}");
            return;
        }
        // Web link on a remote session: the relay can put it in front of the
        // user's own browser, exactly like a terminal link.
        if self.drop_relay_active() || is_remote_session() {
            self.open_detected_url(target);
            return;
        }
        self.status = match open_url(target) {
            Ok(()) => format!("Opened {target}"),
            Err(e) => format!("Open link failed: {e}"),
        };
    }

    /// Drain the LSP fold-span replies (#254) into every editor showing
    /// the file. Seq-gated like inlay hints; an EMPTY reply is dropped so
    /// a server that answers before indexing (or lacks real spans) never
    /// erases the indentation/region fallback — the outline's guard.
    pub fn drain_lsp_folding_ranges(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_folding_ranges() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            if u.ranges.is_empty() {
                continue;
            }
            if self.editor.path.as_deref() == Some(u.path.as_path()) {
                self.editor.set_lsp_folds(u.ranges.clone());
                changed = true;
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(u.path.as_path()) {
                    group.set_lsp_folds(u.ranges.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    /// Drain document-color batches (#254) into every editor showing the
    /// file — the inlay-hints fan-out, seq gate included. An empty batch
    /// clears the swatches (a stylesheet that lost its last color).
    pub fn drain_lsp_document_colors(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_document_colors() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            changed |= self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_document_colors(u.path.clone(), u.colors.clone())
            });
        }
        changed
    }

    /// "Change Color Presentation" (#254): ask for the alternative
    /// spellings of the color value at the cursor and defer the picker on
    /// the reply.
    pub(in crate::app) fn change_color_presentation(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let Some(item) = self.editor.color_at(row, col).cloned() else {
            self.status = String::from("No color value at the cursor");
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let seq = self.editor.edit_seq;
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_color_presentations(
            path.clone(),
            item.raw,
            (item.line, item.character),
            (item.end_line, item.end_character),
        );
        self.color_presentations_request = Some((id, path, seq));
        self.status = String::from("Fetching color presentations…");
    }

    /// Open the presentation picker when the reply lands (#254); gated on
    /// id + path + edit_seq like every deferred caret request.
    pub fn drain_lsp_color_presentations(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_color_presentations() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, seq)) = self.color_presentations_request.clone() else {
                continue;
            };
            if result.request_id != id {
                continue;
            }
            self.color_presentations_request = None;
            if self.editor.path.as_deref() != Some(path.as_path()) || self.editor.edit_seq != seq {
                continue;
            }
            changed = true;
            if result.presentations.is_empty() {
                self.status = String::from("No color presentations offered here");
                continue;
            }
            let rows: Vec<crate::widgets::list_picker::ListRow> = result
                .presentations
                .iter()
                .enumerate()
                .map(|(i, (label, _))| crate::widgets::list_picker::ListRow {
                    id: i.to_string(),
                    label: label.clone(),
                })
                .collect();
            self.pending_color_presentations = result.presentations;
            self.pending_color_context = Some((path.clone(), seq));
            self.open_list_picker(
                crate::widgets::list_picker::ListPicker::new(
                    crate::widgets::list_picker::ListPurpose::ColorPresentation,
                    "Change Color Presentation",
                    rows,
                ),
                "No color presentations offered here",
            );
        }
        changed
    }
}
