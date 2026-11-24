//! Peek Definition and Peek References: the popup with its numbered excerpt, stepping
//! through the references, and closing it.

use super::*;

impl App {
    /// Build and show the Peek Definition popup: a `path:line` header and a
    /// numbered excerpt of the target region with the definition line marked.
    /// The open buffer supplies the text when the target file is already open
    /// (unsaved edits visible); disk otherwise.
    pub(in crate::app) fn open_peek_popup(&mut self, path: PathBuf, line: u32, col: u32) {
        self.open_peek_popup_titled(path, line, col, None);
    }

    /// [`open_peek_popup`](Self::open_peek_popup) with an optional first
    /// line above the location, e.g. "Reference 2 of 7" (#616).
    pub(in crate::app) fn open_peek_popup_titled(
        &mut self,
        path: PathBuf,
        line: u32,
        col: u32,
        title: Option<String>,
    ) {
        // Any open buffer wins over disk — the ACTIVE group's tabs plus
        // every inactive split leaf's — so unsaved edits show wherever the
        // target file happens to be open.
        let open_buffer = self
            .editor
            .editors
            .iter()
            .chain(
                self.editor_layout
                    .inactive_groups()
                    .flat_map(|tabs| tabs.editors.iter()),
            )
            .find(|e| e.path.as_deref() == Some(path.as_path()))
            .map(|e| e.lines.clone());
        let lines: Vec<String> = match open_buffer {
            Some(lines) => lines,
            None => match std::fs::read_to_string(&path) {
                Ok(text) => text.lines().map(|s| s.to_string()).collect(),
                Err(e) => {
                    self.status = format!("Peek failed: {e}");
                    return;
                }
            },
        };
        let target = line as usize;
        let (start, excerpt) = peek_excerpt(&lines, target, 4, 12);
        let mut body: Vec<String> = title.into_iter().collect();
        body.push(format!("{}:{}", self.status_path(&path), target + 1));
        let num_w = (start + excerpt.len()).to_string().len();
        for (i, text) in excerpt.iter().enumerate() {
            let n = start + i;
            let marker = if n == target { '\u{25b6}' } else { ' ' };
            body.push(format!("{marker}{:>num_w$}  {text}", n + 1));
        }
        body.push(String::from("Enter jumps \u{00b7} Esc closes"));
        let anchor = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        self.peek_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
            body.join("\n"),
            anchor,
        ));
        self.peek_target = Some((path, line, col));
        self.status = String::new();
    }

    pub(in crate::app) fn close_peek_popup(&mut self) {
        self.peek_popup = None;
        self.peek_target = None;
        self.peek_refs = None;
    }

    /// Show the reference `peek_refs` points at in the peek popup (#616),
    /// titled with its place in the list.
    pub(in crate::app) fn show_peeked_reference(&mut self) {
        let Some((targets, idx)) = self.peek_refs.clone() else {
            return;
        };
        let Some((path, line, col)) = targets.get(idx).cloned() else {
            return;
        };
        let title = format!(
            "Reference {} of {}  \u{2191}\u{2193} steps",
            idx + 1,
            targets.len()
        );
        self.open_peek_popup_titled(path, line, col, Some(title));
        // Re-opening goes through the same path as a fresh peek, which
        // leaves the reference list alone; restore it in case it was reset.
        self.peek_refs = Some((targets, idx));
    }

    /// Step Peek References by `delta`, wrapping at either end.
    pub(in crate::app) fn step_peeked_reference(&mut self, delta: isize) {
        if let Some((targets, idx)) = self.peek_refs.as_mut() {
            let n = targets.len() as isize;
            *idx = (*idx as isize + delta).rem_euclid(n) as usize;
        }
        self.show_peeked_reference();
    }

    /// Peek References (Alt+Shift+F12, #616): the references of the symbol
    /// at the caret in the peek popup rather than a picker.
    pub(in crate::app) fn peek_references_at_cursor(&mut self) {
        self.request_references_at_cursor();
        self.references_want_peek = self.references_request_id.is_some();
    }
}

impl App {
    /// Alt+F12 / palette "Peek Definition": the definition request whose
    /// response opens an excerpt popup instead of jumping (#115).
    pub(in crate::app) fn peek_definition_at_cursor(&mut self) {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_definition(path, line, character);
        self.peek_definition_request_id = Some(id);
        self.status = String::from("Peeking definition");
    }
}

/// The excerpt window for a Peek popup: up to `total` lines, at most
/// `above` of them before `target`, clamped to the file. Returns the start
/// line index and the excerpt slice (cloned).
pub(in crate::app) fn peek_excerpt(
    lines: &[String],
    target: usize,
    above: usize,
    total: usize,
) -> (usize, Vec<String>) {
    if lines.is_empty() {
        return (0, Vec::new());
    }
    let target = target.min(lines.len() - 1);
    let start = target.saturating_sub(above);
    let end = (start + total).min(lines.len());
    let start = end.saturating_sub(total).min(start);
    (start, lines[start..end].to_vec())
}
