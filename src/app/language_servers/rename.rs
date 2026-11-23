//! Rename Symbol (F2): the prompt, pre-filled from prepare-rename or the word at the caret,
//! the rename request, and landing its reply.

use super::*;

impl App {
    /// VS Code "Rename Symbol" (F2): prompt for a new name pre-filled with the
    /// identifier under the cursor; committing fires an LSP `rename` request.
    pub(in crate::app) fn start_rename_symbol(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        // Prepare-rename first (#254): the server validates the position
        // and hands back the exact range + placeholder, so the prompt
        // pre-fills the real symbol and an invalid position fails fast
        // with the server's own message. The worker always answers;
        // an `unsupported` verdict falls back to the word-under-cursor
        // prompt below (today's behavior, kept for servers without the
        // capability).
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let (line, character) = self.editor.pos_to_utf16(row, col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_prepare_rename(path.clone(), line, character);
        // Overwriting also disarms any previous in-flight prepare: its caret
        // is abandoned, and a late verdict for it must not reopen a prompt
        // carrying the old (row, col) into the rename request.
        self.prepare_rename_request = Some((id, path, row, col, self.editor.edit_seq));
        self.status = String::from("Preparing rename…");
    }

    /// The pre-#254 rename prompt: pre-fill with the word under the
    /// cursor. The fallback when no server advertises `prepareProvider`
    /// (or it answers "default behavior").
    pub(in crate::app) fn open_rename_prompt_fallback(
        &mut self,
        path: PathBuf,
        row: usize,
        col: usize,
    ) {
        let Some((start, end)) = self.editor.word_at(row, col) else {
            self.status = String::from("No symbol under cursor");
            return;
        };
        let word: String = self.editor.lines[row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        self.prompt = Some(Prompt {
            label: format!("Rename Symbol '{word}'"),
            buffer: word,
            caret: usize::MAX,
            kind: PromptKind::RenameSymbol { path, row, col },
            target_dir: PathBuf::new(),
            error: None,
        });
    }

    /// Resolve a prepare-rename verdict (#254) into the prompt, an
    /// error, or the fallback. Gated on id + path + edit_seq like every
    /// deferred caret request.
    pub fn drain_lsp_prepare_rename(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_prepare_rename() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            changed |= self.apply_prepare_rename_verdict(result);
        }
        changed
    }

    /// Resolve one prepare-rename verdict into the prompt, a status, or the
    /// word-under-cursor fallback. Split out of the drain loop so the four
    /// verdict shapes are testable: the reply channel's sender lives inside
    /// the LSP worker, so a test cannot inject a `PrepareRenameResult`
    /// through it. Returns whether the screen needs a repaint.
    pub(in crate::app) fn apply_prepare_rename_verdict(
        &mut self,
        result: crate::lsp::manager::PrepareRenameResult,
    ) -> bool {
        let Some((id, path, row, col, seq)) = self.prepare_rename_request.clone() else {
            return false;
        };
        if result.request_id != id {
            return false;
        }
        self.prepare_rename_request = None;
        // The verdict is in, whichever way it went: "Preparing rename…" no
        // longer describes anything (it stayed up beside the open prompt).
        if self.status == "Preparing rename…" {
            self.status.clear();
        }
        if self.editor.path.as_deref() != Some(path.as_path()) || self.editor.edit_seq != seq {
            // The caret moved on (an edit, or a tab switch — which also
            // bumps edit_seq). Say so: bailing silently would strand
            // "Preparing rename…" on screen with no way to tell the
            // rename died.
            self.status = String::from("Rename cancelled");
            return true;
        }
        if result.unsupported {
            self.open_rename_prompt_fallback(path, row, col);
            return true;
        }
        if let Some(msg) = result.error {
            self.status = format!("Rename rejected: {msg}");
            return true;
        }
        let Some(((sl, sc, el, ec), placeholder)) = result.target else {
            self.status = String::from("Nothing renameable at the cursor");
            return true;
        };
        // Placeholder wins; otherwise the validated range's text.
        let word = placeholder.unwrap_or_else(|| {
            let (sr, er) = (sl as usize, el as usize);
            let scol = self.editor.utf16_col_to_char_pub(sr, sc);
            let ecol = self.editor.utf16_col_to_char_pub(er, ec);
            if sr == er {
                self.editor
                    .lines
                    .get(sr)
                    .map(|l| {
                        l.chars()
                            .skip(scol)
                            .take(ecol.saturating_sub(scol))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            }
        });
        if word.is_empty() {
            self.open_rename_prompt_fallback(path, row, col);
            return true;
        }
        self.prompt = Some(Prompt {
            label: format!("Rename Symbol '{word}'"),
            buffer: word,
            caret: usize::MAX,
            kind: PromptKind::RenameSymbol { path, row, col },
            target_dir: PathBuf::new(),
            error: None,
        });
        true
    }

    pub fn drain_lsp_rename(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let Some(reply) = take_reply(
            &mut self.rename_request_id,
            || lsp.drain_rename(),
            |r| r.request_id,
        ) else {
            return false;
        };
        self.land_rename(reply.edits);
        true
    }

    /// Apply a rename reply, unless a buffer it edits was typed into after
    /// the request went out (#1151): its positions were computed against
    /// the text as it was then, and would land on whatever sits there now.
    pub(in crate::app) fn land_rename(
        &mut self,
        edits: Option<Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)>>,
    ) {
        let baseline = std::mem::take(&mut self.rename_baseline);
        match edits {
            Some(files) => {
                if let Some(file) = self.stale_edit_target(&baseline, &files) {
                    self.status = format!(
                        "Rename cancelled: {file} changed while the server was working; press F2 again"
                    );
                    return;
                }
                match self.apply_rename_edits(&files) {
                    Ok((file_count, occ)) => {
                        self.status =
                            format!("Renamed {occ} occurrence(s) across {file_count} file(s)");
                    }
                    Err(e) => self.status = format!("Rename failed: {e}"),
                }
            }
            None => self.status = String::from("Rename produced no changes"),
        }
    }
}
