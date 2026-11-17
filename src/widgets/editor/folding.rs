//! Folding and indent guides: fold ranges from indentation, language servers and #region
//! markers, hidden lines, and the guides drawn at each indent.

use super::*;

impl Editor {
    /// Leading-whitespace width of `line`, or `None` for a blank line (a blank
    /// line belongs to whatever block surrounds it, so it carries no indent of
    /// its own). Tabs and spaces each count as one column, which is all the
    /// fold heuristic needs: it only ever compares indents with `>`.
    pub(super) fn indent_width(&self, line: usize) -> Option<usize> {
        let s = self.lines.get(line)?;
        (!s.trim().is_empty()).then(|| s.chars().take_while(|c| *c == ' ' || *c == '\t').count())
    }

    /// The foldable region headed by `line`, as `(line, last_line_inclusive)`,
    /// or `None` when nothing below `line` is more indented. Interior blank
    /// lines are absorbed; trailing blanks and the first line back at (or
    /// below) the header's indent are not. This is VS Code's default
    /// indentation-based folding: a pure function of the text, no LSP or
    /// tree-sitter round-trip.
    pub fn fold_range(&self, line: usize) -> Option<(usize, usize)> {
        // Server spans first (#254): while a capable server has answered
        // for this exact line count, its ranges REPLACE the heuristics
        // wholesale — VS Code's provider model. rust-analyzer and friends
        // emit region markers and comment spans themselves.
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .map(|r| (line, r.end_line.min(self.lines.len().saturating_sub(1))))
                .max_by_key(|&(_, end)| end);
        }
        // Fallback #1: the marker/comment table (`#region` pairs, comment
        // runs) — folds indentation alone can't see.
        if let Some(&(s, e, _)) = self
            .fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
        {
            return Some((s, e.min(self.lines.len().saturating_sub(1))));
        }
        // Fallback #2: the indentation scan.
        self.indent_fold_range(line)
    }

    /// The plain indentation-based fold heuristic, a pure function of the
    /// text — the last-resort scanner behind the server spans and the
    /// marker/comment table.
    pub(super) fn indent_fold_range(&self, line: usize) -> Option<(usize, usize)> {
        let base = self.indent_width(line)?;
        let mut end = line;
        let mut i = line + 1;
        while i < self.lines.len() {
            match self.indent_width(i) {
                None => {} // blank: possibly interior, keep scanning
                Some(w) if w > base => end = i,
                Some(_) => break, // back to base indent or shallower
            }
            i += 1;
        }
        (end > line).then_some((line, end))
    }

    /// Whether `line` heads a collapsible region.
    pub fn is_foldable(&self, line: usize) -> bool {
        self.fold_range(line).is_some()
    }

    /// Server fold spans, but only while they still describe this buffer:
    /// an edit changes the line count and orphans them until the next
    /// reply (same whole-buffer posture as `fold_epoch_lines`).
    pub(super) fn lsp_folds_current(&self) -> Option<&[crate::lsp::manager::FoldingRangeItem]> {
        match &self.lsp_folds {
            Some(ranges) if self.lsp_folds_lines == self.lines.len() => Some(ranges),
            _ => None,
        }
    }

    /// Install a server's fold-span set (#254). The caller (the app's LSP
    /// drain) already seq-gated the reply; the line count is recorded so
    /// later edits orphan the spans instead of folding the wrong lines.
    pub fn set_lsp_folds(&mut self, ranges: Vec<crate::lsp::manager::FoldingRangeItem>) {
        self.lsp_folds = Some(ranges);
        self.lsp_folds_lines = self.lines.len();
        // Spans under existing collapsed headers may have moved.
        if !self.folded.is_empty() {
            self.rebuild_hidden_ranges();
        }
    }

    /// Rebuild the fallback marker/comment fold table when the buffer
    /// changed. Cheap no-op per frame otherwise; called from the render
    /// and from every fold command so headless callers agree with paint.
    pub fn refresh_fold_tables(&mut self) {
        if self.fallback_folds_seq == Some(self.edit_seq) {
            return;
        }
        self.fallback_folds_seq = Some(self.edit_seq);
        self.fallback_kind_folds.clear();
        use crate::lsp::manager::FoldRangeKind;
        // Region markers: a stack pairs nested #region/#endregion.
        let mut stack: Vec<usize> = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            match region_marker(l) {
                Some(true) => stack.push(i),
                Some(false) => {
                    if let Some(start) = stack.pop()
                        && i > start
                    {
                        self.fallback_kind_folds
                            .push((start, i, FoldRangeKind::Region));
                    }
                }
                None => {}
            }
        }
        // Comment runs: ≥2 consecutive full-line comments fold as one
        // block from the first line.
        let mut run_start: Option<usize> = None;
        for i in 0..=self.lines.len() {
            let is_comment = self
                .lines
                .get(i)
                .is_some_and(|l| comment_line(l) && region_marker(l).is_none());
            match (run_start, is_comment) {
                (None, true) => run_start = Some(i),
                (Some(s), false) => {
                    if i - s >= 2 {
                        self.fallback_kind_folds
                            .push((s, i - 1, FoldRangeKind::Comment));
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
        self.fallback_kind_folds
            .sort_unstable_by_key(|&(s, e, _)| (s, e));
    }

    /// The fold kind at a header line, when one is known: the server's
    /// kind, else the fallback table's. Plain indentation folds have no
    /// kind — "Fold All Comments/Regions" skips them.
    pub fn fold_kind_at(&self, line: usize) -> Option<crate::lsp::manager::FoldRangeKind> {
        if let Some(ranges) = self.lsp_folds_current() {
            return ranges
                .iter()
                .filter(|r| r.start_line == line)
                .max_by_key(|r| r.end_line)
                .map(|r| r.kind);
        }
        self.fallback_kind_folds
            .iter()
            .find(|&&(s, _, _)| s == line)
            .map(|&(_, _, k)| k)
    }

    /// Collapse every fold of `kind` — "Fold All Comments" (Cmd+K Cmd+/) /
    /// "Fold All Regions" (Cmd+K Cmd+8).
    pub fn fold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let headers: Vec<usize> = (0..self.lines.len())
            .filter(|&l| self.fold_kind_at(l) == Some(kind) && self.is_foldable(l))
            .collect();
        if headers.is_empty() {
            return;
        }
        self.folded.extend(headers);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.is_line_hidden(self.cursor_row) {
            self.cursor_row = self.cursor_row.saturating_sub(1);
        }
    }

    /// Expand every collapsed fold of `kind` — "Unfold All Regions"
    /// (Cmd+K Cmd+9). The mirror of [`Self::fold_all_of_kind`].
    pub fn unfold_all_of_kind(&mut self, kind: crate::lsp::manager::FoldRangeKind) {
        self.refresh_fold_tables();
        let drop: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&l| self.fold_kind_at(l) == Some(kind))
            .collect();
        if drop.is_empty() {
            return;
        }
        for l in drop {
            self.folded.remove(&l);
        }
        self.rebuild_hidden_ranges();
    }

    /// The OUTERMOST fold header whose region covers `line` — the function's
    /// `def`/`fn` line rather than the nearest `if`/`for` block that
    /// [`Self::enclosing_fold_header`] stops at. Climbs header-by-header;
    /// each step strictly decreases the line, so it terminates.
    pub(super) fn outermost_enclosing_header(&self, line: usize) -> Option<usize> {
        let mut head = self.enclosing_fold_header(line)?;
        loop {
            let above = (0..head)
                .rev()
                .find(|&h| matches!(self.fold_range(h), Some((_, end)) if head <= end));
            match above {
                Some(a) => head = a,
                None => return Some(head),
            }
        }
    }

    /// Column distance between indentation guides: the indent width for
    /// space-indented buffers, one column for tab indentation (a leading tab
    /// occupies a single cell, so each tab is one nesting level).
    pub(super) fn guide_step(&self) -> usize {
        let s = self.indent_style();
        if s.use_spaces {
            s.width.max(1) as usize
        } else {
            1
        }
    }

    /// Indent width `line` contributes to guide painting. A non-blank line is
    /// its own leading-whitespace width; a blank line borrows the minimum of
    /// its nearest non-blank neighbours, so guides run through gaps inside a
    /// block but stop between top-level blocks (VS Code's rule). The
    /// neighbour scans are capped: past a 200-blank-line gap guides simply
    /// stop, which keeps the per-row render cost bounded.
    pub(super) fn guide_indent_width(&self, line: usize) -> usize {
        self.guide_indent(line).0
    }

    /// [`Self::guide_indent_width`] plus the line whose leading whitespace
    /// that width counts: the line itself, or for a blank line the neighbour
    /// it borrowed from. The width is in characters, so where its guides sit
    /// on screen has to be measured on that line (a tab is several cells; a
    /// blank line has none of its own to measure).
    pub(super) fn guide_indent(&self, line: usize) -> (usize, usize) {
        const BLANK_SCAN_CAP: usize = 200;
        if let Some(w) = self.indent_width(line) {
            return (w, line);
        }
        let above = (0..line)
            .rev()
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i).map(|w| (w, i)));
        let below = (line + 1..self.lines.len())
            .take(BLANK_SCAN_CAP)
            .find_map(|i| self.indent_width(i).map(|w| (w, i)));
        match (above, below) {
            (Some(a), Some(b)) => {
                if b.0 < a.0 {
                    b
                } else {
                    a
                }
            }
            _ => (0, line),
        }
    }

    /// The active indentation guide (VS Code
    /// `editor.guides.highlightActiveIndentation`): the innermost guide of the
    /// block containing the cursor, as `(guide column, first line, last line)`
    /// inclusive. A line whose next non-blank neighbour is deeper is a block
    /// header and activates the guide of the body it opens. `None` at top
    /// level. The block walk is capped so a pathological single block can't
    /// make a frame scan the whole file.
    pub(super) fn active_indent_guide(&self) -> Option<(usize, usize, usize)> {
        const BLOCK_WALK_CAP: usize = 5_000;
        let step = self.guide_step();
        let cr = self.cursor_row.min(self.lines.len().saturating_sub(1));
        let own = self
            .indent_width(cr)
            .unwrap_or_else(|| self.guide_indent_width(cr));
        let below = (cr + 1..self.lines.len())
            .take(BLOCK_WALK_CAP)
            .find_map(|i| self.indent_width(i))
            .unwrap_or(0);
        let target = own.max(below);
        if target == 0 {
            return None;
        }
        // The innermost guide of a block indented `target`: the last guide
        // column strictly below it.
        let col = (target - 1) / step * step;
        // Anchor on a row that actually draws the guide: the cursor row for a
        // body line, the first body row for a header.
        let anchor = if own > col { cr } else { cr + 1 };
        if anchor >= self.lines.len() || self.guide_indent_width(anchor) <= col {
            return None;
        }
        let mut lo = anchor;
        while lo > 0 && anchor - (lo - 1) <= BLOCK_WALK_CAP && self.guide_indent_width(lo - 1) > col
        {
            lo -= 1;
        }
        let mut hi = anchor;
        while hi + 1 < self.lines.len()
            && (hi + 1) - anchor <= BLOCK_WALK_CAP
            && self.guide_indent_width(hi + 1) > col
        {
            hi += 1;
        }
        Some((col, lo, hi))
    }

    /// The nearest foldable header at or above `line` whose region contains
    /// `line`, so "fold at cursor" collapses the enclosing block when the
    /// cursor sits in a body line rather than on the header itself.
    pub(super) fn enclosing_fold_header(&self, line: usize) -> Option<usize> {
        if self.is_foldable(line) {
            return Some(line);
        }
        (0..line)
            .rev()
            .find(|&h| matches!(self.fold_range(h), Some((_, end)) if line <= end))
    }

    /// How many lines apart `a` and `b` are on screen: the lines between
    /// them, less those a collapsed fold hides.
    pub(super) fn visible_line_distance(&self, a: usize, b: usize) -> usize {
        let (lo, hi) = (a.min(b), a.max(b));
        let hidden: usize = self
            .hidden_ranges
            .iter()
            .map(|&(header, end)| {
                let (from, to) = ((header + 1).max(lo + 1), end.min(hi));
                (to + 1).saturating_sub(from)
            })
            .sum();
        (hi - lo).saturating_sub(hidden)
    }

    /// Whether `line` is hidden inside a collapsed region. A binary search over
    /// the merged spans in [`Self::hidden_ranges`], which the fold writes keep
    /// current — this is called for every rendered row of every frame.
    pub fn is_line_hidden(&self, line: usize) -> bool {
        self.hidden_ranges
            .binary_search_by(|&(header, end)| {
                if line <= header {
                    std::cmp::Ordering::Greater
                } else if line > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Recompute [`Self::hidden_ranges`] from `folded`. Every write to `folded`
    /// must call this. Headers are visited in ascending order and any header
    /// already inside the span being built is skipped: an inner fold's range is
    /// contained by its outer's, so it can add nothing.
    pub(super) fn rebuild_hidden_ranges(&mut self) {
        // Hidden lines take no wrap rows, so the cached totals change too.
        self.wrap_total_cache.clear();
        #[cfg(test)]
        FOLD_RANGE_REBUILDS.with(|c| c.set(c.get() + 1));
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &h in &self.folded {
            if let Some(&(_, end)) = out.last()
                && h <= end
            {
                continue;
            }
            if let Some(span) = self.fold_range(h) {
                out.push(span);
            }
        }
        self.hidden_ranges = out;
    }

    /// Drop every fold that touches rows `start..=end`, before an edit that
    /// reorders those rows without changing the line count. Folds are line
    /// numbers and are only reset when the count changes, so after Move Line
    /// or Sort Lines a fold kept hiding the same numbers over different text.
    pub(super) fn forget_folds_in(&mut self, start: usize, end: usize) {
        if self.folded.is_empty() {
            return;
        }
        let touched: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&h| {
                (start..=end).contains(&h)
                    || self
                        .fold_range(h)
                        .is_some_and(|(s, e)| s <= end && e >= start)
            })
            .collect();
        if touched.is_empty() {
            return;
        }
        for h in touched {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Unfold whatever collapsed region the cursor is sitting inside. Movement
    /// is not the only way in — search, go-to-definition and goto-line all set
    /// `cursor_row` directly — and a caret on a hidden line cannot be painted
    /// at all (`cursor_screen_pos` has no row for it), so an edit there would
    /// change content the user cannot see. Cheap when nothing is folded.
    pub fn reveal_cursor_fold(&mut self) {
        if self.folded.is_empty() {
            return;
        }
        let row = self.cursor_row;
        let covering: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|&h| matches!(self.fold_range(h), Some((_, end)) if h < row && row <= end))
            .collect();
        if covering.is_empty() {
            // Nothing to reveal, so the spans are still current. `pin_on_edit`
            // calls this on every mutation: rebuilding here would rescan the
            // whole buffer once per keystroke for every folded header, which
            // after Fold All is the expensive case this cache exists to avoid.
            return;
        }
        for h in covering {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Next visible line at or after `line`, walking out of any collapsed
    /// region. `None` past the end of the buffer.
    pub(super) fn next_visible_line(&self, line: usize) -> Option<usize> {
        let mut l = line;
        while l < self.lines.len() {
            if !self.is_line_hidden(l) {
                return Some(l);
            }
            l += 1;
        }
        None
    }

    /// Previous visible line at or before `line`.
    pub(super) fn prev_visible_line(&self, line: usize) -> usize {
        let mut l = line;
        while l > 0 && self.is_line_hidden(l) {
            l -= 1;
        }
        l
    }

    /// Toggle the fold owning `line` (VS Code `editor.toggleFold`). Collapsing a
    /// block that contains the cursor snaps the cursor up to the header so it
    /// never strands on a hidden line.
    pub fn toggle_fold(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let collapsing = !self.folded.remove(&header);
        if collapsing {
            self.folded.insert(header);
        }
        // Both branches changed the fold set, so both must refresh the spans.
        self.rebuild_hidden_ranges();
        if collapsing && self.is_line_hidden(self.cursor_row) {
            let len = self
                .lines
                .get(header)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            self.cursor_row = header;
            self.cursor_col = self.cursor_col.min(len);
        }
        self.fold_epoch_lines = self.lines.len();
    }

    /// Collapse every foldable region (VS Code "Fold All", Cmd+K Cmd+0),
    /// snapping the cursor up to the nearest visible line.
    pub fn fold_all(&mut self) {
        self.refresh_fold_tables();
        self.folded = (0..self.lines.len())
            .filter(|&l| self.is_foldable(l))
            .collect();
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
    }

    /// Every foldable header with its range's end and its nesting level, in
    /// line order (#1653): level 1 is outermost, and a header inside another
    /// header's range is one level deeper.
    pub(super) fn fold_headers_with_levels(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        // The ends of the ranges enclosing the line being scanned.
        let mut open: Vec<usize> = Vec::new();
        for header in 0..self.lines.len() {
            let Some((_, end)) = self.fold_range(header) else {
                continue;
            };
            while open.last().is_some_and(|&e| e < header) {
                open.pop();
            }
            out.push((header, end, open.len() + 1));
            open.push(end);
        }
        out
    }

    /// Collapse every fold exactly `level` deep and leave the others as they
    /// are (VS Code "Fold Level N", Cmd+K Cmd+N, #1653): a deeper fold is
    /// hidden inside its collapsed parent anyway, and keeping its own state
    /// means opening the parent again shows it as it was. Fold
    /// Level 2 on a module keeps its classes and their methods' headers in
    /// view with the bodies collapsed, the overview Fold All cannot give
    /// since it collapses the outermost block too. The cursor snaps up to the
    /// nearest visible line, as for Fold All.
    pub fn fold_level(&mut self, level: usize) {
        self.refresh_fold_tables();
        let headers: Vec<usize> = self
            .fold_headers_with_levels()
            .into_iter()
            .filter(|&(_, _, l)| l == level)
            .map(|(h, _, _)| h)
            .collect();
        if headers.is_empty() {
            return;
        }
        self.folded.extend(headers);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        while self.cursor_row > 0 && self.is_line_hidden(self.cursor_row) {
            self.cursor_row -= 1;
        }
        let len = self.line_char_len(self.cursor_row);
        self.cursor_col = self.cursor_col.min(len);
    }

    /// Collapse the fold owning `line` and every fold inside it (VS Code
    /// "Fold Recursively", #1653), so unfolding it later shows its inner
    /// blocks collapsed. The cursor snaps to the header when it was inside.
    pub fn fold_recursively(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let Some((start, end)) = self.fold_range(header) else {
            return;
        };
        let inner: Vec<usize> = (start..=end).filter(|&l| self.is_foldable(l)).collect();
        self.folded.extend(inner);
        self.fold_epoch_lines = self.lines.len();
        self.rebuild_hidden_ranges();
        if self.is_line_hidden(self.cursor_row) {
            self.cursor_row = header;
            let len = self.line_char_len(header);
            self.cursor_col = self.cursor_col.min(len);
        }
    }

    /// Expand the fold owning `line` and every fold inside it (VS Code
    /// "Unfold Recursively", #1653). Folds outside its range stay as they
    /// are.
    pub fn unfold_recursively(&mut self, line: usize) {
        self.refresh_fold_tables();
        let Some(header) = self.enclosing_fold_header(line) else {
            return;
        };
        let Some((start, end)) = self.fold_range(header) else {
            return;
        };
        let inner: Vec<usize> = self
            .folded
            .iter()
            .copied()
            .filter(|h| (start..=end).contains(h))
            .collect();
        if inner.is_empty() {
            return;
        }
        for h in inner {
            self.folded.remove(&h);
        }
        self.rebuild_hidden_ranges();
    }

    /// Expand every fold (VS Code "Unfold All", Cmd+K Cmd+J).
    pub fn unfold_all(&mut self) {
        self.folded.clear();
        self.hidden_ranges.clear();
    }

    /// The last line `row`'s fold hides, when `row` is a folded header.
    pub(super) fn folded_end(&self, row: usize) -> Option<usize> {
        self.hidden_ranges
            .iter()
            .find(|&&(header, _)| header == row)
            .map(|&(_, end)| end)
    }

    /// The header of the fold hiding `row`, when a fold hides it.
    pub(super) fn folded_header_hiding(&self, row: usize) -> Option<usize> {
        self.hidden_ranges
            .iter()
            .find(|&&(header, end)| header < row && row <= end)
            .map(|&(header, _)| header)
    }

    /// Carry the folds through a line move that sent each row `r` to
    /// `map(r)`: a moved fold stays folded over the same text. `before` is
    /// the fold set ahead of the move. It replaces what the edit's own fold
    /// sync made of the move, which follows a header by its text and so
    /// loses one whose text repeats. The server's spans move too, since a
    /// move keeps the line count they are checked against; one the move
    /// split apart is dropped until the next reply.
    pub(super) fn move_folds(
        &mut self,
        before: std::collections::BTreeSet<usize>,
        map: impl Fn(usize) -> usize,
    ) {
        self.folded = before.into_iter().map(&map).collect();
        if let Some(spans) = self.lsp_folds.as_mut() {
            for span in spans.iter_mut() {
                span.start_line = map(span.start_line);
                span.end_line = map(span.end_line);
            }
            spans.retain(|span| span.start_line < span.end_line);
        }
        // The marker and comment table is rebuilt from the moved text.
        self.refresh_fold_tables();
        self.rebuild_hidden_ranges();
    }
}

/// Classify a line as a fold-region marker (#254): after leading
/// whitespace and a comment introducer (`//`, `#`, `--`, `;`, `/*`, with
/// an optional extra `#` as in `// #region`), the word `region`
/// (Some(true)) or `endregion` (Some(false)) at a word boundary — the
/// language-agnostic `#region` family VS Code folds.
pub(super) fn region_marker(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let mut rest = None;
    for intro in ["//", "#", "--", ";", "/*"] {
        if let Some(r) = trimmed.strip_prefix(intro) {
            rest = Some(r);
            break;
        }
    }
    let s = rest?.trim_start();
    let s = s.strip_prefix('#').unwrap_or(s);
    let boundary = |r: &str| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_');
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("endregion") {
        return boundary(&s[9..]).then_some(false);
    }
    if lower.starts_with("region") {
        return boundary(&s[6..]).then_some(true);
    }
    None
}

/// Whether a line is a full-line comment for the fallback comment-run
/// scanner. Rust/C attributes (`#[...]`, `#![...]`) are code, not
/// comments, despite the leading `#`.
pub(super) fn comment_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("#[") || t.starts_with("#![") {
        return false;
    }
    ["//", "#", "--", ";", "/*", "*/", "* "]
        .iter()
        .any(|intro| t.starts_with(intro))
        || t == "*"
}
