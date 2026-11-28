//! Syntax colouring: highlight passes on and off the UI thread, carrying colours across an
//! edit, bracket-pair colours and colour swatches.

use super::*;

impl Editor {
    /// Recompute this editor's syntax spans against the current global palette.
    /// Called on a theme switch: cached spans carry baked colors, so without
    /// this the open file keeps the old theme's code colors until the next edit.
    pub fn rehighlight_for_theme(&mut self) {
        // A pass already running bakes the old palette's colors.
        self.highlight_job = None;
        self.recompute_highlights();
        // A diff view's colours are baked the same way (#2020).
        if let Some(diff) = self.diff.as_mut() {
            diff.syntax.restart();
        }
        // The markdown preview bakes the theme into its lines at build time
        // and rebuilds only when the buffer edits (`built_seq`), so a theme
        // switch must rebuild it here or it keeps the old colors until the
        // user touches the file. Requires `self.theme` to already carry the
        // new theme (the caller assigns it first).
        if let Some(doc) = self
            .markdown_preview
            .as_ref()
            .and_then(|md| md.doc_path.clone())
        {
            let is_media = self.markdown_preview.as_ref().is_some_and(|md| md.media);
            let scroll = self
                .markdown_preview
                .as_ref()
                .map(|m| m.scroll)
                .unwrap_or(0);
            // Media cards re-probe headers; documents re-walk XML (#183).
            let rebuilt = if is_media {
                self.open_media_preview(&doc).is_ok()
            } else {
                self.open_doc_preview(&doc).is_ok()
            };
            if rebuilt && let Some(md) = self.markdown_preview.as_mut() {
                md.scroll = scroll;
            }
        } else if self.markdown_preview.as_ref().is_some_and(|md| md.notebook) {
            self.rebuild_notebook_preview();
        } else if self.markdown_preview.is_some() {
            self.rebuild_markdown_preview();
        }
    }

    /// Bring syntax colors, bracket colors and the other per-line decorations
    /// up to date after an edit (or a language or theme change). The syntax
    /// pass runs on a worker thread (#850): the current spans are carried
    /// through the edit at once, and the pass's own replace them when it
    /// lands, within this call if it fits the budget, else on a later tick.
    pub(super) fn recompute_highlights(&mut self) {
        let wait = match self.highlight_budget {
            Some(budget) if self.highlight_cost <= budget => Some(budget),
            Some(_) => Some(std::time::Duration::ZERO),
            None => None,
        };
        self.recompute_highlights_within(wait);
    }

    /// [`Self::recompute_highlights`], waiting at most `wait` for the syntax
    /// pass (`None`: until it finishes).
    pub(super) fn recompute_highlights_within(&mut self, wait: Option<std::time::Duration>) {
        self.highlight_requests = self.highlight_requests.wrapping_add(1);
        self.protected_current = false;
        match self.lang {
            Some(kind) => {
                carry_highlights(
                    &mut self.highlight_base,
                    &mut self.highlights,
                    &mut self.bracket_colors,
                    &self.lines,
                );
                // A pass for another language colors nothing this buffer shows.
                if self
                    .highlight_job
                    .as_ref()
                    .is_some_and(|job| job.kind != kind)
                {
                    self.highlight_job = None;
                }
                // One in flight already picks this edit up when it lands.
                if self.highlight_job.is_none() {
                    self.start_highlight_pass(kind);
                }
                self.await_highlight_pass(wait);
            }
            None => {
                self.highlight_job = None;
                self.highlight_base = PackedLines::default();
                self.highlights = self.plain_log_highlights();
                self.protected_ranges.clear();
                // No grammar means no string/comment knowledge; brackets in
                // plain text still colorize (VS Code does the same) — unless
                // the buffer is big enough that the per-edit scan would make
                // typing in a large log pay for every character.
                let bytes: usize = self.lines.iter().map(String::len).sum();
                self.bracket_colors = if bytes > BRACKET_SCAN_MAX_BYTES {
                    Vec::new()
                } else {
                    scan_bracket_colors(&self.lines, &[])
                };
            }
        }
        let total_bytes: usize = self.lines.iter().map(String::len).sum();
        self.color_swatches = if total_bytes > COLOR_SWATCH_SCAN_MAX_BYTES {
            Vec::new()
        } else {
            scan_color_swatches(&self.lines, self.swatch_decimal_short())
        };
        self.recompute_semantic_overlay();
        self.recompute_diagnostic_spans();
        self.recompute_inlay_spans();
    }

    /// Whether a short run of decimal digits after `#` is a colour here:
    /// only in stylesheet-like buffers (elsewhere it is an issue reference).
    pub(super) fn swatch_decimal_short(&self) -> bool {
        matches!(
            self.lang,
            Some(LangKind::Css | LangKind::Html | LangKind::Vue | LangKind::Svelte)
        )
    }

    /// Hand the buffer's text to a worker thread for a syntax pass as `kind`.
    pub(super) fn start_highlight_pass(&mut self, kind: LangKind) {
        let text = self.lines.join("\n");
        let line_lens = embedded_line_lens(&self.lines);
        let (tx, rx) = std::sync::mpsc::channel();
        // Under test a thread can carry a code palette of its own (the
        // screen goldens' themes), which the pass reads on its worker.
        #[cfg(test)]
        let palette = crate::highlight::thread_syntax_palette();
        let spawned = std::thread::Builder::new()
            .name(String::from("croft-highlight"))
            .spawn(move || {
                #[cfg(test)]
                crate::highlight::set_thread_syntax_palette(palette);
                // Nobody listening (the tab closed, or a newer pass replaced
                // this one) is fine: the result is simply dropped.
                let _ = tx.send(run_highlight_pass(kind, text, line_lens));
            });
        let request = self.highlight_requests;
        match spawned {
            Ok(_) => self.highlight_job = Some(HighlightJob { kind, request, rx }),
            // No thread to be had: highlight here, as before #850.
            Err(_) => {
                let pass = run_highlight_pass(
                    kind,
                    self.lines.join("\n"),
                    embedded_line_lens(&self.lines),
                );
                self.land_highlight_pass(pass, request);
            }
        }
    }

    /// Wait up to `wait` (`None`: however long it takes) for the pass in
    /// flight, and for the one it starts if edits overtook it.
    pub(super) fn await_highlight_pass(&mut self, wait: Option<std::time::Duration>) {
        use std::sync::mpsc::RecvTimeoutError;
        // A zero budget leaves the pass to the tick, even one already done.
        if wait == Some(std::time::Duration::ZERO) {
            return;
        }
        let deadline = wait.map(|wait| std::time::Instant::now() + wait);
        while let Some(job) = &self.highlight_job {
            let request = job.request;
            let received = match deadline {
                None => job.rx.recv().ok(),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    match job.rx.recv_timeout(left) {
                        Ok(pass) => Some(pass),
                        Err(RecvTimeoutError::Timeout) => return,
                        Err(RecvTimeoutError::Disconnected) => None,
                    }
                }
            };
            match received {
                Some(pass) => {
                    self.land_highlight_pass(pass, request);
                }
                // The worker died without a result; the next edit retries.
                None => self.highlight_job = None,
            }
        }
    }

    /// Swap in the background syntax pass if it has finished (#850). The app
    /// calls this every tick for every tab; true when the colors changed.
    pub fn poll_highlights(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        // A diff view's colours land the same way (#2020). One whose pass
        // waited for a free slot starts it once one frees.
        if let Some(diff) = self.diff.as_mut() {
            if diff.syntax.waiting() {
                diff.colour_syntax(Some(std::time::Duration::ZERO));
            }
            if diff.syntax.poll() {
                return true;
            }
        }
        let Some(job) = &self.highlight_job else {
            return false;
        };
        let request = job.request;
        match job.rx.try_recv() {
            Ok(pass) => self.land_highlight_pass(pass, request),
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.highlight_job = None;
                false
            }
        }
    }

    /// Bumped each time a background syntax pass lands.
    pub fn highlight_generation(&self) -> u64 {
        self.highlight_generation
    }

    /// Take a finished pass's spans and bracket colors, carried through any
    /// edits made while it ran, and start the next pass if there were any.
    /// False when the buffer has left the pass's language since it started.
    pub(super) fn land_highlight_pass(&mut self, pass: HighlightPass, request: u64) -> bool {
        self.highlight_job = None;
        if self.lang != Some(pass.kind) {
            return false;
        }
        self.highlight_cost = pass.cost;
        self.highlights = pass.spans;
        self.bracket_colors = pass.brackets;
        // String and comment ranges hold only for the text the pass read.
        self.protected_current = pass.lines == self.lines[..];
        self.protected_ranges = if self.protected_current {
            pass.protected
        } else {
            Vec::new()
        };
        self.highlight_base = pass.lines;
        carry_highlights(
            &mut self.highlight_base,
            &mut self.highlights,
            &mut self.bracket_colors,
            &self.lines,
        );
        self.highlight_generation = self.highlight_generation.wrapping_add(1);
        if request != self.highlight_requests {
            self.start_highlight_pass(pass.kind);
        }
        true
    }

    /// Whether a background syntax pass is still out.
    #[cfg(test)]
    pub fn highlight_pending(&self) -> bool {
        self.highlight_job.is_some()
    }

    /// Stop edits waiting for the syntax pass: each lands only through
    /// `poll_highlights`, as it does on a file too large for the budget.
    #[cfg(test)]
    pub fn defer_highlights_for_test(&mut self) {
        self.highlight_budget = Some(std::time::Duration::ZERO);
    }

    #[cfg(test)]
    pub fn highlights_for_test(&self) -> &[Vec<HiSpan>] {
        &self.highlights
    }

    /// Line `line`'s syntax spans as `(start, end, style)`, for tests
    /// outside this module (#862).
    #[cfg(test)]
    pub fn line_spans_for_test(&self, line: usize) -> Vec<(usize, usize, Style)> {
        self.highlights
            .get(line)
            .map(|spans| {
                spans
                    .iter()
                    .map(|s| (s.range().start, s.range().end, s.style))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The buffer text to key a semantic-token cache entry on, but only when
    /// the buffer is clean: a dirty buffer no longer matches the on-disk
    /// content the next open will read, so caching it would key the entry under
    /// the wrong content. Returns the same normalised (`\n`-joined) text the
    /// app sends to the server, so the key matches on both store and load.
    pub fn clean_cache_text(&self) -> Option<String> {
        (!self.dirty).then(|| self.document_text())
    }
}

/// The whole-identifier tokens of `line`, in order: maximal
/// `[A-Za-z_][A-Za-z0-9_]*` runs, so an inline-value lookup for `x` can
/// never match the `x` inside `max` (#135).
pub(super) fn identifier_tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        let is_ident = ch == '_' || ch.is_ascii_alphanumeric();
        match (start, is_ident) {
            (None, true) if ch == '_' || ch.is_ascii_alphabetic() => start = Some(i),
            (Some(s), false) => {
                out.push(&line[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

/// Colour-index marker for an unmatched closing bracket (painted red).
pub(super) const UNEXPECTED_BRACKET: u8 = u8::MAX;

/// Per line, the `(char column, colour index)` of every colored bracket.
/// The column is a `u32` (#1707): 8 bytes a bracket rather than 16, and no
/// line the editor holds is four billion characters long.
pub(super) type BracketColors = Vec<Vec<(u32, u8)>>;

/// A bracket's char column as [`BracketColors`] holds it.
pub(super) fn bracket_col(col: usize) -> u32 {
    u32::try_from(col).unwrap_or(u32::MAX)
}

/// Plain-text buffers above this size skip bracket colorization: a
/// no-grammar file has no tree-sitter pass absorbing a per-edit full scan,
/// so typing in a large log must not pay O(total chars) per keystroke.
/// Grammar-backed buffers are not gated here: their scan runs inside the
/// syntax pass, on its worker thread (#850).
pub(super) const BRACKET_SCAN_MAX_BYTES: usize = 1_000_000;

/// Nesting-depth colours cycle through this many entries (VS Code's default
/// themes define three `editorBracketHighlight` foregrounds).
pub(super) const BRACKET_COLOR_CYCLE: usize = 3;

/// Bracket-pair colorization scan: per line, the `(char column, colour
/// index)` of every `()[]{}` outside `protected` (absolute byte ranges of
/// strings and comments, ascending — the grammar's view from
/// `highlight_text_with_protected`). One shared depth counter across the
/// three bracket kinds (VS Code's default pool); an opener colours at its own
/// depth, a matching closer at the opener's depth, and a closer that matches
/// nothing — or not the innermost open bracket — marks `UNEXPECTED_BRACKET`
/// while leaving the stack alone, so `(]` reddens the `]` and a later `)`
/// still pairs with the `(`.
pub(super) fn scan_bracket_colors<S: AsRef<str>>(
    lines: &[S],
    protected: &[(usize, usize)],
) -> BracketColors {
    let mut out = vec![Vec::new(); lines.len()];
    let mut stack: Vec<char> = Vec::new();
    let mut abs = 0usize;
    let mut pi = 0usize;
    for (li, line) in lines.iter().enumerate() {
        for (ci, ch) in line.as_ref().chars().enumerate() {
            let b = abs;
            abs += ch.len_utf8();
            while pi < protected.len() && protected[pi].1 <= b {
                pi += 1;
            }
            if pi < protected.len() && protected[pi].0 <= b {
                continue;
            }
            match ch {
                '(' | '[' | '{' => {
                    out[li].push((bracket_col(ci), (stack.len() % BRACKET_COLOR_CYCLE) as u8));
                    stack.push(ch);
                }
                ')' | ']' | '}' => {
                    let want = match ch {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if stack.last() == Some(&want) {
                        stack.pop();
                        out[li].push((bracket_col(ci), (stack.len() % BRACKET_COLOR_CYCLE) as u8));
                    } else {
                        out[li].push((bracket_col(ci), UNEXPECTED_BRACKET));
                    }
                }
                _ => {}
            }
        }
        abs += 1; // the '\n' separator between joined lines
    }
    out
}

/// How long an edit waits for its background syntax pass before painting
/// with the spans carried through it (#850). Half a 60 Hz frame: a pass that
/// fits colors the keystroke in the same frame, as before; a longer one
/// lands on a later tick instead of holding the keystroke.
pub(super) const HIGHLIGHT_INLINE_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

/// A background syntax pass in flight (#850): the receiving end, and what it
/// was started for.
pub(super) struct HighlightJob {
    kind: LangKind,
    /// `Editor::highlight_requests` when the pass started. A later request
    /// (an edit) makes its result stale, so another pass follows it.
    request: u64,
    rx: std::sync::mpsc::Receiver<HighlightPass>,
}

/// The byte length of each of `lines`, when one of them holds a `\n` of its
/// own (one typed as a character), so a pass over their joined text can tell
/// the editor's lines from the grammar's. `None` in the usual case, where
/// splitting the joined text on `\n` gives the lines back, so starting a pass
/// costs no per-line allocation.
pub(super) fn embedded_line_lens(lines: &[String]) -> Option<Vec<usize>> {
    lines
        .iter()
        .any(|l| l.contains('\n'))
        .then(|| lines.iter().map(String::len).collect())
}

/// Highlight `text` as `kind` and scan its brackets outside the strings and
/// comments the grammar found: everything `recompute_highlights` used to do
/// inline, now run on a worker thread (#850). `line_lens` is
/// [`embedded_line_lens`] of the lines `text` joins.
pub(super) fn run_highlight_pass(
    kind: LangKind,
    text: String,
    line_lens: Option<Vec<usize>>,
) -> HighlightPass {
    let started = std::time::Instant::now();
    let line_starts = compute_line_starts(text.as_bytes());
    let (mut spans, protected) = crate::highlight::highlight_text_with_protected(
        &mut LangRegistry::new(),
        kind,
        text.as_bytes(),
        &line_starts,
    );
    // A line can hold a `\n` of its own (one typed as a character), which
    // the grammar reads as a line break. Fold those pieces' spans back onto
    // their line, so the pass describes the editor's lines exactly: a pass
    // over text that has not changed since then lands as is, instead of being
    // carried through a line diff whose time budget makes the result vary run
    // to run.
    let lines = match line_lens {
        None => PackedLines::from_joined(text),
        Some(lens) => {
            let mut at = 0;
            let lines: Vec<&str> = lens
                .iter()
                .map(|&len| {
                    let line = &text[at..at + len];
                    at += len + 1;
                    line
                })
                .collect();
            let mut pieces = spans.into_iter();
            spans = lines
                .iter()
                .map(|line| {
                    let mut out = Vec::new();
                    let mut offset = 0;
                    for piece in line.split('\n') {
                        out.extend(pieces.next().unwrap_or_default().into_iter().map(|s| {
                            let r = s.range();
                            HiSpan::new(r.start + offset, r.end + offset, s.style)
                        }));
                        offset += piece.len() + 1;
                    }
                    out
                })
                .collect();
            PackedLines::from(&lines[..])
        }
    };
    let mut brackets = scan_bracket_colors(&lines.iter().collect::<Vec<_>>(), &protected);
    // The result lives as long as the tab (#1707): each line's list grew by
    // doubling, which left about a third of it spare. Trimmed here, on the
    // worker, rather than on the UI thread that keeps it.
    spans.iter_mut().for_each(Vec::shrink_to_fit);
    brackets.iter_mut().for_each(Vec::shrink_to_fit);
    HighlightPass {
        kind,
        lines,
        spans,
        brackets,
        protected,
        cost: started.elapsed(),
    }
}

/// How long [`carry_highlights`] may spend diffing the edited lines. An edit
/// touches a line or a few, which diffs in microseconds; this caps the
/// pathological case (a tab reused for a different file), where a coarser
/// diff only means fewer lines keep their colors until the pass lands.
pub(super) const CARRY_DIFF_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

/// Carry syntax spans and bracket colours computed for `base` over to
/// `lines`, the same buffer after some edits (#850), and make `base` match
/// `lines`. A line diff pairs old lines with new: an unchanged line keeps its
/// colors, a changed one carries what the change left alone (see
/// [`carry_changed_lines`]), and an inserted one has none until the next
/// pass. Every offset stays inside its line and on a char boundary. False
/// when `base` already matches `lines`.
pub(super) fn carry_highlights(
    base: &mut PackedLines,
    spans: &mut Vec<Vec<HiSpan>>,
    brackets: &mut BracketColors,
    lines: &[String],
) -> bool {
    use similar::DiffOp;
    if spans.len() != base.len() || brackets.len() != base.len() {
        // Not computed for `base`, so there is nothing sound to carry.
        *spans = vec![Vec::new(); lines.len()];
        *brackets = vec![Vec::new(); lines.len()];
        *base = PackedLines::from(lines);
        return true;
    }
    let prefix = base
        .iter()
        .zip(lines)
        .take_while(|(old, new)| *old == new.as_str())
        .count();
    if prefix == base.len() && prefix == lines.len() {
        return false;
    }
    let room = base.len().min(lines.len()) - prefix;
    let suffix = base
        .iter()
        .rev()
        .zip(lines.iter().rev())
        .take(room)
        .take_while(|(old, new)| *old == new.as_str())
        .count();
    let (old_end, new_end) = (base.len() - suffix, lines.len() - suffix);
    let old: Vec<&str> = base.lines_in(prefix..old_end).collect();
    let new: Vec<&str> = lines[prefix..new_end].iter().map(String::as_str).collect();
    let (old_spans, old_brackets) = (&spans[prefix..old_end], &brackets[prefix..old_end]);
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &old,
        &new,
        Some(std::time::Instant::now() + CARRY_DIFF_BUDGET),
    );
    let mut mid_spans = Vec::with_capacity(new.len());
    let mut mid_brackets = Vec::with_capacity(new.len());
    for op in ops {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                let kept = old_index..old_index + len;
                mid_spans.extend_from_slice(&old_spans[kept.clone()]);
                mid_brackets.extend_from_slice(&old_brackets[kept]);
            }
            DiffOp::Delete { .. } => {}
            DiffOp::Insert { new_len, .. } => {
                mid_spans.extend(std::iter::repeat_n(Vec::new(), new_len));
                mid_brackets.extend(std::iter::repeat_n(Vec::new(), new_len));
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // As many lines as before: each was edited in place (several
                // carets, an indent, a replace-all), so each carries its own.
                let groups: Vec<(usize, usize, usize, usize)> = if old_len == new_len {
                    (0..old_len)
                        .map(|k| (old_index + k, 1, new_index + k, 1))
                        .collect()
                } else {
                    vec![(old_index, old_len, new_index, new_len)]
                };
                for (oi, ol, ni, nl) in groups {
                    let (s, b) = carry_changed_lines(
                        &old[oi..oi + ol],
                        &new[ni..ni + nl],
                        &old_spans[oi..oi + ol],
                        &old_brackets[oi..oi + ol],
                    );
                    mid_spans.extend(s);
                    mid_brackets.extend(b);
                }
            }
        }
    }
    debug_assert_eq!(mid_spans.len(), new.len());
    spans.splice(prefix..old_end, mid_spans);
    brackets.splice(prefix..old_end, mid_brackets);
    base.splice(prefix..old_end, &new);
    true
}

/// One changed stretch of [`carry_highlights`]: `old` became `new`, whole
/// lines each. Both are joined and compared as text to find the one change
/// between them. A span wholly before or after the change keeps its color
/// (moved by the change's length); one the change falls strictly inside
/// stretches over it, as does one that text is typed right after on the same
/// line; one the change cuts keeps its surviving part; and the rest was
/// replaced, so it goes. The new text is otherwise uncolored until the pass lands. Spans are
/// then re-split at `new`'s line ends. A bracket keeps its colour wherever
/// the change left it.
pub(super) fn carry_changed_lines(
    old: &[&str],
    new: &[&str],
    spans: &[Vec<HiSpan>],
    brackets: &[Vec<(u32, u8)>],
) -> (Vec<Vec<HiSpan>>, BracketColors) {
    let mut out_spans = vec![Vec::new(); new.len()];
    let mut out_brackets = vec![Vec::new(); new.len()];
    if new.is_empty() {
        return (out_spans, out_brackets);
    }
    let old_text = old.join("\n");
    let new_text = new.join("\n");
    let (head, tail) = common_affixes(&old_text, &new_text);
    let old_change_end = old_text.len() - tail;
    let new_change_end = new_text.len() - tail;
    let inserted_only = head == old_change_end;
    // Text typed onto the end of a span, on its line, joins the span.
    let grows = inserted_only && !new_text[head..new_change_end].contains('\n');
    let shift = |at: usize| at - old_change_end + new_change_end;
    let carry_span = |start: usize, end: usize| -> Option<(usize, usize)> {
        if end < head || (end == head && !grows) {
            Some((start, end))
        } else if end == head {
            // Typed right after it: the word, comment or string being
            // written keeps its color while it grows.
            Some((start, new_change_end))
        } else if start >= old_change_end {
            Some((shift(start), shift(end)))
        } else if start < head && end > old_change_end {
            Some((start, shift(end)))
        } else if start < head {
            Some((start, head))
        } else if end > old_change_end {
            Some((new_change_end, shift(end)))
        } else {
            None
        }
    };
    let new_starts: Vec<usize> = new
        .iter()
        .scan(0usize, |next, line| {
            let at = *next;
            *next += line.len() + 1;
            Some(at)
        })
        .collect();
    let line_of = |at: usize| new_starts.partition_point(|&s| s <= at) - 1;
    // Brackets land in ascending order, so each new column is counted on
    // from the previous bracket's (line, byte, column) rather than from the
    // start of its line, which made a minified line quadratic (#1214).
    let mut cursor = (usize::MAX, 0usize, 0usize);
    let mut old_start = 0usize;
    for (i, line) in old.iter().enumerate() {
        for span in &spans[i] {
            let end = span.range().end.min(line.len());
            if span.range().start >= end {
                continue;
            }
            let Some((mut from, to)) = carry_span(old_start + span.range().start, old_start + end)
            else {
                continue;
            };
            let mut li = line_of(from);
            while from < to && li < new.len() {
                let line_start = new_starts[li];
                let piece_end = to.min(line_start + new[li].len());
                if piece_end > from {
                    out_spans[li].push(HiSpan::new(
                        from - line_start,
                        piece_end - line_start,
                        span.style,
                    ));
                }
                li += 1;
                if let Some(&next) = new_starts.get(li) {
                    from = from.max(next);
                }
            }
        }
        // Columns ascend, so one pass over the line's chars finds them all.
        let mut chars = line.char_indices().enumerate();
        for &(col, depth) in &brackets[i] {
            let Some((_, (byte, _))) = chars.find(|(c, _)| *c == col as usize) else {
                break;
            };
            let at = old_start + byte;
            if at >= head && at < old_change_end {
                continue;
            }
            let moved = if at < head { at } else { shift(at) };
            let li = line_of(moved);
            let in_line = moved - new_starts[li];
            if cursor.0 != li || cursor.1 > in_line {
                cursor = (li, 0, 0);
            }
            cursor.2 += new[li][cursor.1..in_line].chars().count();
            cursor.1 = in_line;
            out_brackets[li].push((bracket_col(cursor.2), depth));
        }
        old_start += line.len() + 1;
    }
    (out_spans, out_brackets)
}

pub(super) fn common_affixes(a: &str, b: &str) -> (usize, usize) {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let mut head = ab.iter().zip(bb).take_while(|(x, y)| x == y).count();
    while !a.is_char_boundary(head) {
        head -= 1;
    }
    let room = ab.len().min(bb.len()) - head;
    let mut tail = ab
        .iter()
        .rev()
        .zip(bb.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while !a.is_char_boundary(a.len() - tail) {
        tail -= 1;
    }
    (head, tail)
}

/// Perceived luminance (0..=255) of an sRGB triple, used to pick a legible
/// black-or-white foreground for a color swatch's background tint.
pub(super) fn swatch_luma(r: u8, g: u8, b: u8) -> f32 {
    0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
}

pub(super) fn swatch_fg_for_bg(r: u8, g: u8, b: u8) -> Color {
    if swatch_luma(r, g, b) >= 140.0 {
        Color::Black
    } else {
        Color::White
    }
}

pub(super) fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse a `#`-prefixed hex color (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`)
/// starting at byte `i` in `line`. Returns the parsed RGB and the number of
/// bytes consumed (including the `#`), or `None` if `i` isn't a `#` followed
/// by 3/4/6/8 hex digits with no further hex digit (or word char) right
/// after — so `#deadbeef123` (too long to be any CSS hex form) is left
/// alone rather than truncated to a wrong prefix.
///
/// Two look-alikes are refused. A `#` glued to a word character or `&` is a
/// URL fragment (`page#fade`) or an HTML entity (`&#123;`). A short run of
/// decimal digits only (`#555`, `#1234`) is an issue or PR reference, which
/// comments and Markdown are full of; it counts as a color only where
/// `decimal_short` says the buffer is a stylesheet.
pub(super) fn parse_hex_color(
    bytes: &[u8],
    i: usize,
    decimal_short: bool,
) -> Option<((u8, u8, u8), usize)> {
    if bytes.get(i) != Some(&b'#') {
        return None;
    }
    if i > 0 && (matches!(bytes[i - 1], b'&' | b'_') || bytes[i - 1].is_ascii_alphanumeric()) {
        return None;
    }
    let mut len = 0usize;
    while bytes
        .get(i + 1 + len)
        .copied()
        .is_some_and(|b| hex_digit(b).is_some())
    {
        len += 1;
        if len > 8 {
            break;
        }
    }
    // The digit run must end exactly at 3, 4, 6, or 8 — a longer run (or one
    // immediately followed by another word character) is not a CSS hex color.
    if !matches!(len, 3 | 4 | 6 | 8) {
        return None;
    }
    if bytes
        .get(i + 1 + len)
        .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    if !decimal_short
        && matches!(len, 3 | 4)
        && bytes[i + 1..=i + len].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let d = |k: usize| hex_digit(bytes[i + 1 + k]).unwrap();
    let rgb = match len {
        3 | 4 => (d(0) * 17, d(1) * 17, d(2) * 17),
        _ => (d(0) * 16 + d(1), d(2) * 16 + d(3), d(4) * 16 + d(5)),
    };
    Some((rgb, 1 + len))
}

/// Parse `rgb(`/`rgba(` (case-insensitive) starting at byte `i`, tolerating
/// the CSS Color 4 comma-optional syntax and a trailing `/ alpha`. Returns
/// the parsed RGB and the byte length of the whole `rgb(...)` call.
pub(super) fn parse_rgb_color(line: &str, i: usize) -> Option<((u8, u8, u8), usize)> {
    let rest = &line[i..];
    let lower_prefix_len = if rest
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("rgb("))
    {
        4
    } else if rest
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("rgba("))
    {
        5
    } else {
        return None;
    };
    let close = rest.find(')')?;
    let inner = &rest[lower_prefix_len..close];
    let mut nums = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|s| !s.is_empty());
    let r: u8 = nums.next()?.parse().ok()?;
    let g: u8 = nums.next()?.parse().ok()?;
    let b: u8 = nums.next()?.parse().ok()?;
    Some(((r, g, b), close + 1))
}

/// One integer of a code-level color call: decimal or `0x` hex, `_`
/// separators allowed, an optional `u8` suffix (`0xeb`, `235`, `0xEB_u8`).
fn parse_color_component(text: &str) -> Option<u8> {
    let t = text.trim();
    let t = t.strip_suffix("u8").unwrap_or(t).trim_end_matches('_');
    let t: String = t.chars().filter(|&c| c != '_').collect();
    let v = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => t.parse().ok()?,
    };
    u8::try_from(v).ok()
}

/// Parse a Rust-style color constructor starting at byte `i`:
/// `Color::Rgb(0xeb, 0xcb, 0x8b)`, `Rgb(235, 203, 139)`, `Color::from_rgb(..)`,
/// a bare `from_rgb(..)`, and the alpha forms (`Rgba(..)`,
/// `from_rgba_unmultiplied(..)`) whose fourth value is read but not drawn.
/// The values may be wrapped in an array, as the `image` crate's
/// `Rgba([0xff, 0, 0, 0xff])` has them. Every value is an integer literal.
/// The whole call, path included, is the match. It must start at a path
/// start (not glued to a word, `_` or `::`), so `x::Rgb(` is left alone.
fn parse_code_rgb_color(line: &str, i: usize) -> Option<((u8, u8, u8), usize)> {
    const PREFIXES: [&str; 8] = [
        "Color::Rgba(",
        "Color::Rgb(",
        "Color::from_rgb(",
        "Color::from_rgba_unmultiplied(",
        "Rgba(",
        "Rgb(",
        "from_rgba_unmultiplied(",
        "from_rgb(",
    ];
    let rest = &line[i..];
    let prefix = PREFIXES.iter().find(|p| rest.starts_with(**p))?;
    if i > 0 {
        let prev = line.as_bytes()[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b':' {
            return None;
        }
    }
    let args = &rest[prefix.len()..];
    let (inner, consumed) = match args.trim_start().strip_prefix('[') {
        Some(array) => {
            let end = array.find(']')?;
            let tail = array[end + 1..].trim_start().strip_prefix(')')?;
            (&array[..end], rest.len() - tail.len())
        }
        None => {
            let end = args.find(')')?;
            (&args[..end], prefix.len() + end + 1)
        }
    };
    let mut parts = inner.split(',');
    let r = parse_color_component(parts.next()?)?;
    let g = parse_color_component(parts.next()?)?;
    let b = parse_color_component(parts.next()?)?;
    let alpha = parts.next().filter(|p| !p.trim().is_empty());
    if alpha.is_some_and(|a| parse_color_component(a).is_none()) {
        return None;
    }
    // At most one more (empty) piece, from a trailing comma.
    if parts.any(|p| !p.trim().is_empty()) {
        return None;
    }
    Some(((r, g, b), consumed))
}

/// Convert HSL (h in degrees, s/l as 0..=100 percentages) to sRGB.
pub(super) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let s = s / 100.0;
    let l = l / 100.0;
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue_to_rgb = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    (
        to_u8(hue_to_rgb(h + 1.0 / 3.0)),
        to_u8(hue_to_rgb(h)),
        to_u8(hue_to_rgb(h - 1.0 / 3.0)),
    )
}

/// Parse `hsl(`/`hsla(` (case-insensitive), same comma/space tolerance as
/// [`parse_rgb_color`]. `s` and `l` may carry a trailing `%`, stripped
/// before parsing.
pub(super) fn parse_hsl_color(line: &str, i: usize) -> Option<((u8, u8, u8), usize)> {
    let rest = &line[i..];
    let prefix_len = if rest
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("hsl("))
    {
        4
    } else if rest
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("hsla("))
    {
        5
    } else {
        return None;
    };
    let close = rest.find(')')?;
    let inner = &rest[prefix_len..close];
    let mut nums = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|s| !s.is_empty());
    let h: f32 = nums.next()?.parse().ok()?;
    let s: f32 = nums.next()?.trim_end_matches('%').parse().ok()?;
    let l: f32 = nums.next()?.trim_end_matches('%').parse().ok()?;
    Some((hsl_to_rgb(h, s, l), close + 1))
}

/// Buffers above this size skip the whole-buffer color-swatch scan, for the
/// same reason [`BRACKET_SCAN_MAX_BYTES`] gates bracket colorization: a
/// per-edit, per-line scan must not make typing in a huge file pay for it.
/// Their visible lines are scanned at draw time instead.
pub(super) const COLOR_SWATCH_SCAN_MAX_BYTES: usize = 1_000_000;

/// Inline color-literal decoration scan (the `naumovs.color-highlight`
/// convention): per line, the `(start char col, end char col, background,
/// foreground)` of every `#hex` / `rgb()` / `rgba()` / `hsl()` / `hsla()` /
/// `Color::Rgb(0xeb, 0xcb, 0x8b)` literal found anywhere in the text — comments, strings, and plain prose
/// alike, since (unlike syntax highlighting) a color swatch has no notion of
/// "this isn't code here." A byte-position match is converted to a char
/// column range so the renderer's column math (shared with bracket
/// colorization and indent guides) applies unchanged. `decimal_short` is
/// passed through to [`parse_hex_color`]: true only for stylesheet buffers.
pub(super) fn scan_color_swatches(
    lines: &[String],
    decimal_short: bool,
) -> Vec<Vec<(usize, usize, Color, Color)>> {
    let mut out = vec![Vec::new(); lines.len()];
    for (li, line) in lines.iter().enumerate() {
        let bytes = line.as_bytes();
        // The char column of byte `i`, advanced alongside it: this runs on
        // every recompute over the whole buffer, so no per-line lookup table.
        let mut col = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            let hit = parse_hex_color(bytes, i, decimal_short)
                .or_else(|| parse_code_rgb_color(line, i))
                .or_else(|| parse_rgb_color(line, i))
                .or_else(|| parse_hsl_color(line, i));
            match hit {
                Some(((r, g, b), consumed)) => {
                    // Every literal form is ASCII up to its closing `)`, but
                    // `rgb(` may enclose anything, so count rather than assume.
                    let width = line[i..i + consumed].chars().count();
                    out[li].push((
                        col,
                        col + width,
                        Color::Rgb(r, g, b),
                        swatch_fg_for_bg(r, g, b),
                    ));
                    col += width;
                    i += consumed;
                }
                None => {
                    // Advance by one char (not one byte) to stay on a UTF-8
                    // boundary for the next `line[i..]` slice a color parser
                    // takes.
                    let step = line[i..].chars().next().map_or(1, char::len_utf8);
                    i += step;
                    col += 1;
                }
            }
        }
    }
    out
}
