//! What language servers report, laid over the text: semantic tokens, inlay hints, links,
//! colours, occurrences, diagnostics, inline values, code lenses and ghost text.

use super::*;

impl Editor {
    /// Store a fresh semantic-token batch from the LSP and decode it into
    /// the per-line overlay. Called by the app when `drain_semantic_tokens`
    /// yields a batch for this editor's file.
    pub fn apply_semantic_tokens(
        &mut self,
        path: PathBuf,
        data: Vec<u32>,
        legend: std::sync::Arc<Vec<String>>,
        is_full: bool,
    ) {
        // A viewport-only (range) batch must never clobber a whole-document
        // batch already in place for the same file: the full set colours the
        // off-screen lines the range set omits, so letting a late range reply
        // win would blank them. Full batches always win; a range batch only
        // applies as the first paint, before the full reply arrives.
        let same_file = self.semantic_path.as_deref() == Some(path.as_path());
        if !is_full && same_file && self.semantic_is_full {
            return;
        }
        self.semantic_path = Some(path);
        self.semantic_data = data;
        self.semantic_legend = Some(legend);
        self.semantic_is_full = is_full;
        self.decode_semantic_overlay();
    }

    /// Store a fresh inlay-hint set from the LSP and decode it into per-line
    /// `(char_col, label)` runs. Called by the app when `drain_inlay_hints`
    /// yields a batch for this editor's file (an empty set clears them).
    pub fn apply_inlay_hints(
        &mut self,
        path: PathBuf,
        hints: Vec<crate::lsp::manager::InlayHintItem>,
    ) {
        self.inlay_path = Some(path);
        self.inlay_hints = hints;
        self.recompute_inlay_spans();
    }

    /// Install the server's link set (#254): wholesale replace.
    pub fn apply_document_links(
        &mut self,
        path: PathBuf,
        links: Vec<crate::lsp::manager::DocumentLinkItem>,
    ) {
        self.doc_links_path = Some(path);
        self.doc_links = links;
    }

    /// The link target under `(row, col)` — the Ctrl+click dispatch's
    /// lookup, valid only for the stamped file. Range ends are exclusive
    /// like every LSP range.
    pub fn document_link_at(&self, row: usize, col: usize) -> Option<&str> {
        if self.doc_links_path.as_deref() != self.path.as_deref() {
            return None;
        }
        self.doc_links
            .iter()
            .find(|l| {
                let (sr, er) = (l.line as usize, l.end_line as usize);
                if row < sr || row > er {
                    return false;
                }
                let sc = self
                    .lines
                    .get(sr)
                    .map(|t| utf16_to_char_col(t, l.character))
                    .unwrap_or(0);
                let ec = self
                    .lines
                    .get(er)
                    .map(|t| utf16_to_char_col(t, l.end_character))
                    .unwrap_or(0);
                (row > sr || col >= sc) && (row < er || col < ec)
            })
            .map(|l| l.target.as_str())
    }

    /// Install the document's color values (#254): wholesale replace,
    /// like inlay hints — an empty batch clears the swatches.
    pub fn apply_document_colors(
        &mut self,
        path: PathBuf,
        colors: Vec<crate::lsp::manager::ColorItem>,
    ) {
        self.color_path = Some(path);
        self.color_infos = colors;
        self.recompute_inlay_spans();
    }

    /// The color value whose range contains `(row, col)` — the Change
    /// Color Presentation picker's target lookup.
    pub fn color_at(&self, row: usize, col: usize) -> Option<&crate::lsp::manager::ColorItem> {
        if self.color_path.as_deref() != self.path.as_deref() {
            return None;
        }
        self.color_infos.iter().find(|c| {
            let (sr, er) = (c.line as usize, c.end_line as usize);
            if row < sr || row > er {
                return false;
            }
            let sc = self.utf16_col_to_char_pub(sr, c.character);
            let ec = self.utf16_col_to_char_pub(er, c.end_character);
            (row > sr || col >= sc) && (row < er || col <= ec)
        })
    }

    /// Public UTF-16 → char-column bridge for one row.
    pub fn utf16_col_to_char_pub(&self, row: usize, character: u32) -> usize {
        self.lines
            .get(row)
            .map(|l| utf16_to_char_col(l, character))
            .unwrap_or(0)
    }

    /// Install the occurrences of the symbol under the caret, converting the
    /// server's UTF-16 columns to character columns row by row. A multi-line
    /// occurrence (rare, but legal) tints its first row from the start column,
    /// its last row up to the end column, and any rows between end to end.
    pub fn apply_occurrences(&mut self, items: Vec<crate::lsp::manager::OccurrenceItem>) {
        self.occurrences.clear();
        for item in items {
            for row in item.start_line..=item.end_line {
                let row = row as usize;
                let Some(text) = self.lines.get(row) else {
                    continue;
                };
                let cols = text.chars().count();
                let start = if row as u32 == item.start_line {
                    utf16_to_char_col(text, item.start_char).min(cols)
                } else {
                    0
                };
                let end = if row as u32 == item.end_line {
                    utf16_to_char_col(text, item.end_char).min(cols)
                } else {
                    cols
                };
                if end > start {
                    self.occurrences.push((row, start, end, item.write));
                }
            }
        }
    }

    /// Drop the occurrence tints (caret moved; the app re-requests on idle).
    pub fn clear_occurrences(&mut self) -> bool {
        let had = !self.occurrences.is_empty();
        self.occurrences.clear();
        had
    }

    /// Drop every inlay hint (the "Editor: Toggle Inlay Hints" off switch).
    pub fn clear_inlay_hints(&mut self) {
        self.inlay_hints = Vec::new();
        self.inlay_path = None;
        self.recompute_inlay_spans();
    }

    /// Re-decode the retained hints into per-logical-line `(char_col, label)`
    /// runs against the current buffer. A no-op (clears the runs) unless the
    /// batch belongs to the loaded file. Between an edit and the next reply
    /// the anchors drift with the old positions (clamped to the line), exactly
    /// like VS Code until its refresh lands; the app replaces the set once the
    /// server answers for the new edit seq.
    pub(super) fn recompute_inlay_spans(&mut self) {
        // Hint cells change the display width of their lines.
        self.hscroll_content_cols = None;
        let same_file = self.inlay_path.as_deref() == self.path.as_deref();
        let colors_same_file = self.color_path.as_deref() == self.path.as_deref();
        let hints_live = same_file && !self.inlay_hints.is_empty();
        let colors_live = colors_same_file && !self.color_infos.is_empty();
        let steps_live = self.step_path.is_some()
            && self.step_path.as_deref() == self.path.as_deref()
            && !self.step_marks.is_empty();
        if !hints_live && !colors_live && !steps_live {
            self.inlay_spans = Vec::new();
            return;
        }
        let mut spans: Vec<Vec<(usize, String, Option<Color>)>> =
            vec![Vec::new(); self.lines.len()];
        if hints_live {
            for h in &self.inlay_hints {
                let Some(text) = self.lines.get(h.line as usize) else {
                    continue;
                };
                let col = utf16_to_char_col(text, h.character).min(text.chars().count());
                spans[h.line as usize].push((col, h.label.clone(), None));
            }
        }
        // Document-color swatches (#254): a one-cell `■` virtual span
        // whose foreground IS the color, anchored at the value's start.
        if colors_live {
            for c in &self.color_infos {
                let Some(text) = self.lines.get(c.line as usize) else {
                    continue;
                };
                let col = utf16_to_char_col(text, c.character).min(text.chars().count());
                spans[c.line as usize].push((
                    col,
                    String::from("\u{25a0}"),
                    Some(Color::Rgb(c.red, c.green, c.blue)),
                ));
            }
        }
        // SARIF analysis steps (#577): after the line's own text.
        if steps_live {
            for (line, label) in &self.step_marks {
                let Some(text) = self.lines.get(*line) else {
                    continue;
                };
                spans[*line].push((
                    text.chars().count(),
                    format!("  \u{25c2} {label}"),
                    Some(STEP_MARK_COLOR),
                ));
            }
        }
        for line in &mut spans {
            line.sort_by_key(|(c, _, _)| *c);
        }
        self.inlay_spans = spans;
    }

    /// Show the selected SARIF result's analysis steps that lie in `path`
    /// (#577), or none. A tab showing another file draws nothing.
    pub fn set_step_marks(&mut self, path: Option<PathBuf>, marks: Vec<(usize, String)>) {
        if self.step_path == path && self.step_marks == marks {
            return;
        }
        self.step_path = path;
        self.step_marks = marks;
        self.recompute_inlay_spans();
    }

    /// Inlay hints of logical line `line`, or nothing in wrap mode: the
    /// wrapped row segmentation knows nothing about hint cells.
    /// ponytail: hints skip wrap mode; code files don't wrap by default, and
    /// Markdown (the wrapping default) has no hint-serving server.
    pub(super) fn row_inlay_spans(&self, line: usize) -> &[(usize, String, Option<Color>)] {
        if self.wrap_enabled() {
            return &[];
        }
        self.inlay_spans.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn inlay_spans_for_test(&self, line: usize) -> &[(usize, String, Option<Color>)] {
        self.row_inlay_spans(line)
    }

    #[cfg(test)]
    pub(crate) fn semantic_overlay_for_test(&self) -> &[Vec<HiSpan>] {
        &self.semantic_overlay
    }

    #[cfg(test)]
    pub(crate) fn occurrence_count_for_test(&self) -> usize {
        self.occurrences.len()
    }

    /// Store a fresh diagnostics set for `path` and decode it into per-line
    /// underline runs. Called by the app when `drain_diagnostics` yields a
    /// batch for this editor's file (an empty `diagnostics` clears them, which
    /// is how an "all clear" republish erases the squiggles).
    pub fn apply_diagnostics(
        &mut self,
        path: PathBuf,
        diagnostics: Vec<crate::lsp::manager::Diagnostic>,
    ) {
        self.diagnostics_path = Some(path);
        self.diagnostics = diagnostics;
        self.diagnostics_seq = self.edit_seq;
        self.recompute_diagnostic_spans();
    }

    /// The file the editor's current diagnostics belong to, if any. The app
    /// reads this to re-apply the stored set after a tab switch (diagnostics
    /// are server-pushed, never re-requested on focus like semantic tokens).
    pub fn diagnostics_path(&self) -> Option<&Path> {
        self.diagnostics_path.as_deref()
    }

    /// Clones of the diagnostics whose range intersects the inclusive logical
    /// line span `[start_line, end_line]`. Passed as `codeAction` context so the
    /// language server can attach quick fixes to the errors under the cursor.
    /// Once the buffer has changed since they arrived, they go without their
    /// origin: a fix in a server's `data` would land where the text used to
    /// be, so the server is asked about them as croft shows them (#1030).
    pub fn diagnostics_in_line_range(
        &self,
        start_line: u32,
        end_line: u32,
    ) -> Vec<crate::lsp::manager::Diagnostic> {
        let edited = self.diagnostics_seq != self.edit_seq;
        self.diagnostics
            .iter()
            .filter(|d| d.start_line <= end_line && d.end_line >= start_line)
            .map(|d| crate::lsp::manager::Diagnostic {
                origin: if edited { None } else { d.origin.clone() },
                ..d.clone()
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_spans_for_test(
        &self,
    ) -> &[Vec<(usize, usize, crate::lsp::manager::DiagnosticSeverity)>] {
        &self.diagnostic_spans
    }

    /// A diagnostic position as a row of this buffer (#911): the final
    /// newline the server is sent ends the document on an empty row past
    /// the last one stored, so a position there (a missing token at the end
    /// of the file) is drawn and found at the end of the last row.
    pub(super) fn diagnostic_row(&self, line: u32, ch: u32) -> (usize, u32) {
        let line = line as usize;
        match line == self.lines.len() && self.sends_final_newline() {
            true => (line - 1, u32::MAX),
            false => (line, ch),
        }
    }

    /// Re-decode the retained diagnostics into per-logical-line character runs
    /// against the current buffer. A no-op (clears the runs) unless the batch
    /// belongs to the loaded file, so stale squiggles never bleed across a tab
    /// switch. A zero-width diagnostic (e.g. a "missing token" pointing between
    /// two characters) is widened to one cell so it is still visible, matching
    /// VS Code drawing a squiggle under at least one character.
    ///
    /// Hints follow VS Code (#1522): a tagged hint (unused, unreachable,
    /// deprecated) gets no underline, only its tag's look over the text, and
    /// a plain hint is marked under its first two characters rather than
    /// underlined end to end like a real problem.
    pub(super) fn recompute_diagnostic_spans(&mut self) {
        use crate::lsp::manager::DiagnosticSeverity;
        let same_file = self.diagnostics_path.as_deref() == self.path.as_deref();
        if !same_file || self.diagnostics.is_empty() {
            self.diagnostic_spans = Vec::new();
            self.diagnostic_tag_spans = Vec::new();
            return;
        }
        let mut spans: Vec<Vec<(usize, usize, DiagnosticSeverity)>> =
            vec![Vec::new(); self.lines.len()];
        let mut tag_spans: Vec<Vec<(usize, usize, crate::lsp::manager::DiagnosticTags)>> =
            vec![Vec::new(); self.lines.len()];
        for d in &self.diagnostics {
            let hint = d.severity == DiagnosticSeverity::Hint;
            let (start_line, start_char) = self.diagnostic_row(d.start_line, d.start_char);
            let (end_line, end_char) = self.diagnostic_row(d.end_line, d.end_char);
            // Zip bounds the walk to the buffer, replacing the per-line
            // `lines.get` check a stale diagnostic range would otherwise need.
            for (line, ((text, line_spans), line_tags)) in self
                .lines
                .iter()
                .zip(spans.iter_mut())
                .zip(tag_spans.iter_mut())
                .enumerate()
                .take(end_line + 1)
                .skip(start_line)
            {
                let line_chars = text.chars().count();
                let from = if line == start_line {
                    utf16_to_char_col(text, start_char)
                } else {
                    0
                };
                let to = if line == end_line {
                    utf16_to_char_col(text, end_char)
                } else {
                    line_chars
                };
                // A zero-width tagged diagnostic gets its look on one
                // character, like the one-cell squiggle below, so a tagged
                // point hint is never left unmarked. At the end of a line
                // there is no character to fade, so a hint falls through
                // to its short underline instead.
                let point = start_line == end_line && from == to;
                let tag_to = if point {
                    (from + 1).min(line_chars)
                } else {
                    to
                };
                if d.tags.any() && tag_to > from {
                    line_tags.push((from, tag_to, d.tags));
                }
                if hint && d.tags.any() && (!point || tag_to > from) {
                    continue;
                }
                // Widen an empty run to one cell so a point diagnostic is seen.
                let to = to.max(from + 1);
                if hint {
                    if line == start_line {
                        line_spans.push((from, to.min(from + 2), d.severity));
                    }
                    continue;
                }
                line_spans.push((from, to, d.severity));
            }
        }
        self.diagnostic_spans = spans;
        self.diagnostic_tag_spans = tag_spans;
    }

    /// Decode the semantic-token batch just received against the current
    /// buffer (the reply is only applied while the text is the one it was
    /// measured on). Clears the overlay unless the batch belongs to the file
    /// currently loaded, so a stale batch never colors the wrong file after a
    /// tab switch.
    pub(super) fn decode_semantic_overlay(&mut self) {
        // The first frame does not show a copy of the text, so a launch
        // builds it after that frame (see `settle_semantic_carry`).
        self.decode_semantic_overlay_deferring(crate::startup_gate::first_frame_pending());
    }

    pub(super) fn decode_semantic_overlay_deferring(&mut self, defer_carry: bool) {
        self.semantic_carry_pending = None;
        self.semantic_exact = false;
        let same_file = self.semantic_path.as_deref() == self.path.as_deref();
        let Some(legend) = self.semantic_legend.as_ref().filter(|_| same_file) else {
            self.semantic_overlay = Vec::new();
            self.semantic_base = PackedLines::default();
            return;
        };
        if self.semantic_data.is_empty() {
            self.semantic_overlay = Vec::new();
            self.semantic_base = PackedLines::default();
            return;
        }
        let text = self.lines.join("\n");
        let bytes = text.as_bytes();
        let line_starts = compute_line_starts(bytes);
        self.semantic_overlay =
            decode_semantic_tokens(&self.semantic_data, legend, bytes, &line_starts);
        self.semantic_exact = true;
        if defer_carry {
            self.semantic_base = PackedLines::default();
            self.semantic_carry_brackets = Vec::new();
            self.semantic_carry_pending = Some(self.edit_seq);
        } else {
            self.semantic_base = PackedLines::from(&self.lines[..]);
            self.semantic_carry_brackets = vec![Vec::new(); self.lines.len()];
        }
    }

    /// Build the copy of the text that carrying the overlay through an edit
    /// needs, once the first frame is up. A buffer edited before then has
    /// moved past the text the batch was measured on and drops it; the
    /// server's next reply paints it again.
    pub fn settle_semantic_carry(&mut self) {
        let Some(seq) = self.semantic_carry_pending else {
            return;
        };
        if crate::startup_gate::first_frame_pending() {
            return;
        }
        self.semantic_carry_pending = None;
        if seq == self.edit_seq {
            self.semantic_base = PackedLines::from(&self.lines[..]);
            self.semantic_carry_brackets = vec![Vec::new(); self.lines.len()];
        } else {
            self.semantic_overlay = Vec::new();
            self.semantic_exact = false;
        }
    }

    /// After a theme switch: the overlay's spans carry colours baked from the
    /// old palette. Decode the batch again if it still fits the text; one an
    /// edit has moved is dropped, as it cannot be decoded against the new
    /// text, and the reply to that edit paints it in the new colours.
    pub(super) fn restyle_semantic_overlay(&mut self) {
        if self.semantic_overlay.is_empty() {
            return;
        }
        if self.semantic_exact
            && self
                .semantic_carry_pending
                .is_none_or(|s| s == self.edit_seq)
        {
            self.decode_semantic_overlay_deferring(self.semantic_carry_pending.is_some());
        } else {
            self.semantic_overlay = Vec::new();
            self.semantic_base = PackedLines::default();
            self.semantic_carry_pending = None;
            self.semantic_exact = false;
        }
    }

    /// After an edit: move the overlay with the text it was decoded for. Lines
    /// the edit touched keep the tokens of their unchanged stretches, new
    /// text has none (the syntax colours show) until the server answers.
    pub(super) fn recompute_semantic_overlay(&mut self) {
        if let Some(seq) = self.semantic_carry_pending {
            // Decoded in the first frame, with no copy of the text to carry
            // from: unchanged since, it stays; edited, it is dropped.
            if seq != self.edit_seq {
                self.semantic_carry_pending = None;
                self.semantic_overlay = Vec::new();
                self.semantic_exact = false;
            }
            return;
        }
        if self.semantic_overlay.is_empty() && self.semantic_base.len() == 0 {
            return;
        }
        if self.semantic_path.as_deref() != self.path.as_deref() {
            // Saved under another name: the batch is another file's.
            self.semantic_overlay = Vec::new();
            self.semantic_base = PackedLines::default();
            self.semantic_exact = false;
            return;
        }
        if carry_highlights(
            &mut self.semantic_base,
            &mut self.semantic_overlay,
            &mut self.semantic_carry_brackets,
            &self.lines,
        ) {
            self.semantic_exact = false;
        }
    }

    /// Where the buffer ends, as an LSP position: the end of an inlay-hint
    /// request range, which a strict server rejects past it (#1490).
    pub fn document_end(&self) -> lsp_types::Position {
        let last = self.lines.last().map_or("", String::as_str);
        lsp_types::Position::new(
            self.lines.len().saturating_sub(1) as u32,
            last.encode_utf16().count() as u32,
        )
    }

    /// Rebuild the debugger inline-value trailers (#135) for a stop at
    /// 1-based `stop_line`. Each line from the enclosing function's header
    /// down to the stop that mentions a local as a whole identifier gets a
    /// "name = value" list, first-mention order, capped per line; long
    /// values elide in the middle. Lines past the execution point stay bare
    /// — their state doesn't exist yet, which is VS Code's rule too.
    pub fn set_inline_values_from_locals(&mut self, stop_line: usize, locals: &[(String, String)]) {
        const MAX_ENTRIES_PER_LINE: usize = 4;
        const MAX_VALUE_CHARS: usize = 40;
        const MAX_SPAN_LINES: usize = 200;
        self.inline_values.clear();
        if locals.is_empty() || stop_line == 0 || self.lines.is_empty() {
            return;
        }
        let stop = (stop_line - 1).min(self.lines.len() - 1);
        let from = self
            .outermost_enclosing_header(stop)
            .unwrap_or(stop)
            .max(stop.saturating_sub(MAX_SPAN_LINES));
        for li in from..=stop {
            let line = self.lines[li].clone();
            let mut parts: Vec<String> = Vec::new();
            for token in identifier_tokens(&line) {
                if parts.len() >= MAX_ENTRIES_PER_LINE {
                    break;
                }
                let Some((name, value)) = locals.iter().find(|(n, _)| n == token) else {
                    continue;
                };
                if parts
                    .iter()
                    .any(|p| p.starts_with(name.as_str()) && p[name.len()..].starts_with(" = "))
                {
                    continue;
                }
                let shown: String = if value.chars().count() > MAX_VALUE_CHARS {
                    let head: String = value.chars().take(MAX_VALUE_CHARS / 2).collect();
                    let tail: String = value
                        .chars()
                        .rev()
                        .take(MAX_VALUE_CHARS / 2 - 1)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("{head}\u{2026}{tail}")
                } else {
                    value.clone()
                };
                parts.push(format!("{name} = {shown}"));
            }
            if !parts.is_empty() {
                self.inline_values.insert(li, parts.join(", "));
            }
        }
    }

    /// Paint line `index` of the live inline suggestion (#1625) as its own
    /// row under the caret's line: no line number, the suggestion's text in
    /// the dim italic ghost style, scrolled with the buffer when not
    /// wrapping and clipped at the pane edge.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_ghost_row(
        &self,
        buf: &mut Buffer,
        inner: Rect,
        y: u16,
        gutter_width: u16,
        text_x: u16,
        text_width: u16,
        index: usize,
        wrap: bool,
    ) {
        buf.set_line(
            inner.x,
            y,
            &Line::from(" ".repeat(gutter_width as usize)),
            gutter_width,
        );
        let Some(line) = self.live_ghost().and_then(|t| t.split('\n').nth(index)) else {
            return;
        };
        let skip = if wrap { 0 } else { self.scroll_col };
        // Clipped by terminal cells, not chars: a wide character takes two,
        // and `set_string` would paint past the text column otherwise.
        let mut cells = 0usize;
        let shown: String = line
            .replace('\t', &" ".repeat(self.indent_style().width.max(1) as usize))
            .chars()
            .skip(skip)
            .take_while(|&c| {
                cells += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                cells <= text_width as usize
            })
            .collect();
        buf.set_string(
            text_x,
            y,
            &shown,
            Style::default()
                .fg(self.theme.ignored_fg())
                .add_modifier(Modifier::ITALIC),
        );
    }

    /// The inline suggestion (#607) when it still applies: the caret where it
    /// was asked for and no edit since.
    pub fn live_ghost(&self) -> Option<&str> {
        self.live_ghost_full().map(|g| g.text.as_str())
    }

    /// [`Self::live_ghost`] with the columns of the caret's line it replaces,
    /// when it is a language server's replacement.
    pub fn live_ghost_full(&self) -> Option<&crate::inline_complete::Ghost> {
        let g = self.ghost.as_ref()?;
        (g.row == self.cursor_row && g.col == self.cursor_col && g.seq == self.edit_seq)
            .then_some(g)
    }

    /// Indices of the code lenses on 0-based `line` whose line still reads
    /// as it did when they arrived (#608).
    pub fn lenses_on_line(&self, line: usize) -> Vec<usize> {
        let Some(text) = self.lines.get(line) else {
            return Vec::new();
        };
        self.code_lenses
            .iter()
            .enumerate()
            .filter(|(_, l)| l.line == line && &l.line_text == text)
            .map(|(i, _)| i)
            .collect()
    }

    /// The code lens painted under screen cell `(col, row)` this frame.
    pub fn code_lens_at(&self, col: u16, row: u16) -> Option<usize> {
        self.code_lens_spans
            .iter()
            .find(|(y, xs, _)| *y == row && xs.contains(&col))
            .map(|(_, _, idx)| *idx)
    }

    /// Live Run's trailer for 0-based `line`, when the line still reads as
    /// it did in the run that produced it.
    pub(super) fn live_note(&self, line: usize) -> Option<&crate::live_run::Note> {
        let text = self.lines.get(line)?;
        self.live_run.as_ref()?.note(line, text)
    }
}
