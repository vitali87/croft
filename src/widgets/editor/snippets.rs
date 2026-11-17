//! Snippets and their tab stops, and Emmet: expanding abbreviations, wrapping, and editing
//! the tag at the caret.

use super::*;

impl Editor {
    /// True while a snippet's tab stops are being cycled with Tab.
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }

    /// Abandon any active snippet session (a structural key moved the caret
    /// away, so Tab should return to its normal meaning).
    pub fn cancel_snippet(&mut self) {
        self.snippet = None;
    }

    /// [`Self::expand_snippet_with`] knowing only this buffer's variables.
    #[cfg(test)]
    pub fn expand_snippet(&mut self, body: &str, prefix_len: usize) {
        self.expand_snippet_with(body, prefix_len, &|_| None);
    }

    /// The value of snippet variable `name` this buffer can tell (#914): its
    /// file, the caret's line, its comment syntax, or the clock's. `None`
    /// for any other name.
    pub(super) fn snippet_var(&self, name: &str) -> Option<String> {
        let path = self.path.as_deref();
        let lossy =
            |s: Option<&std::ffi::OsStr>| s.map_or(String::new(), |s| s.to_string_lossy().into());
        let block = block_comment_tokens(self.lang);
        Some(match name {
            "TM_FILENAME" => lossy(path.and_then(Path::file_name)),
            "TM_FILENAME_BASE" => lossy(path.and_then(Path::file_stem)),
            "TM_DIRECTORY" => lossy(path.and_then(Path::parent).map(Path::as_os_str)),
            "TM_FILEPATH" => lossy(path.map(Path::as_os_str)),
            "TM_SELECTED_TEXT" | "SELECTION" => self.selection_text(),
            "TM_CURRENT_LINE" => self.lines.get(self.cursor_row).cloned().unwrap_or_default(),
            "TM_LINE_INDEX" => self.cursor_row.to_string(),
            "TM_LINE_NUMBER" => (self.cursor_row + 1).to_string(),
            "LINE_COMMENT" => self.line_comment_marker().unwrap_or_default().to_string(),
            "BLOCK_COMMENT_START" => block.map_or("", |b| b.0).to_string(),
            "BLOCK_COMMENT_END" => block.map_or("", |b| b.1).to_string(),
            _ => return crate::snippets::clock_var(name),
        })
    }

    /// Expand snippet `body` at the caret. `prefix_len` chars of the already
    /// typed prefix are removed first, then the resolved text is inserted with
    /// continuation lines indented to the current line. The caret lands on the
    /// first tab stop (selecting its placeholder); when more stops remain a
    /// session starts so Tab cycles through them. Variables resolve from this
    /// buffer first, then from `vars`, the workspace's (#914).
    pub fn expand_snippet_with(
        &mut self,
        body: &str,
        prefix_len: usize,
        vars: &dyn Fn(&str) -> Option<String>,
    ) {
        self.expand_snippet_with_in(body, prefix_len, vars, None);
    }

    /// [`Self::expand_snippet_with`] in the undo step `group`: a completion
    /// that is a snippet expands in the step of its accept, which the
    /// auto-import its server resolves afterwards joins too (#928).
    pub fn expand_snippet_with_in(
        &mut self,
        body: &str,
        prefix_len: usize,
        vars: &dyn Fn(&str) -> Option<String>,
        group: Option<u64>,
    ) {
        self.snippet = None;
        for _ in 0..prefix_len {
            self.backspace_in(group);
        }
        let start = (self.cursor_row, self.cursor_col);
        let indent: String = self
            .lines
            .get(start.0)
            .map(|l| {
                l.chars()
                    .take(start.1)
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect()
            })
            .unwrap_or_default();
        let indent_len = indent.chars().count();
        // A value's line breaks are `\n`, as the body's: stops are counted in it.
        let resolve = |name: &str| {
            let value = self.snippet_var(name).or_else(|| vars(name));
            value.map(|v| normalize_newlines(&v))
        };
        let parsed = crate::snippets::parse_body(body, &resolve);
        // A body's leading `\t` is one indent level, VS Code's convention
        // (#1554): it becomes this buffer's indent unit, so a space-indented
        // file gets no tab after its spaces. Tabs past the indentation are
        // text and stay.
        let unit = self.indent_unit();
        let grow = unit.chars().count().saturating_sub(1);
        let body_lines: Vec<&str> = parsed.text.split('\n').collect();
        let leading_tabs = |line: &str, upto: usize| {
            line.chars()
                .take(upto)
                .take_while(|c| *c == ' ' || *c == '\t')
                .filter(|c| *c == '\t')
                .count()
        };
        let text = body_lines
            .iter()
            .map(|line| {
                let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                format!("{}{}", line[..lead].replace('\t', &unit), &line[lead..])
            })
            .collect::<Vec<_>>()
            .join("\n");
        let inserted = if indent.is_empty() {
            text
        } else {
            text.replace('\n', &format!("\n{indent}"))
        };
        // A snippet body is text the user chose, not text they wrote (#349).
        self.insert_str_as_in(&inserted, crate::provenance::Seat::Generated, group);

        let mut abs: std::collections::VecDeque<(usize, usize, usize)> = parsed
            .stops
            .iter()
            .map(|s| {
                let (ld, col) = crate::snippets::offset_to_line_col(&parsed.text, s.offset);
                let col = col + grow * leading_tabs(body_lines[ld], col);
                let row = start.0 + ld;
                let c = if ld == 0 {
                    start.1 + col
                } else {
                    indent_len + col
                };
                (row, c, s.len)
            })
            .collect();
        let Some(first) = abs.pop_front() else {
            return; // no stops: leave the caret at the end of the insertion
        };
        self.place_at_snippet_stop(first);
        if !abs.is_empty() {
            self.snippet = Some(SnippetSession {
                anchor: (first.0, first.1),
                cur_len: first.2,
                cur_text: self.span_text(first.0, first.1, first.2),
                stops: abs,
            });
        }
    }

    /// Advance to the next snippet tab stop. Later stops on the current line are
    /// shifted by the net chars typed at the current stop first. Returns false
    /// when no session is active (so the caller falls back to indenting).
    pub fn snippet_next(&mut self) -> bool {
        let Some(mut sess) = self.snippet.take() else {
            return false;
        };
        // ponytail: only edits made at the current stop, on its own line, are
        // tracked. Typing newlines or editing elsewhere mid-session drifts the
        // later stops; upgrade to real markers if that workflow matters.
        if self.cursor_row == sess.anchor.0 {
            let typed = self.cursor_col as isize - sess.anchor.1 as isize;
            let shift = typed - sess.cur_len as isize;
            let now = self.span_text(sess.anchor.0, sess.anchor.1, typed.max(0) as usize);
            if now != sess.cur_text {
                // Stops nested inside the placeholder just replaced
                // (`${1:foo(${2:x})}`) name text that is gone: drop them, or
                // the next Tab selects whatever now sits at their offset,
                // a same-length replacement included.
                let after = sess.anchor.1 + sess.cur_len;
                let anchor = sess.anchor;
                sess.stops
                    .retain(|s| !(s.0 == anchor.0 && s.1 >= anchor.1 && s.1 < after));
            }
            if shift != 0 {
                let after = sess.anchor.1 + sess.cur_len;
                for s in sess.stops.iter_mut() {
                    if s.0 == sess.anchor.0 && s.1 >= after {
                        s.1 = (s.1 as isize + shift).max(0) as usize;
                    }
                }
            }
        }
        let Some(next) = sess.stops.pop_front() else {
            return false;
        };
        self.place_at_snippet_stop(next);
        if !sess.stops.is_empty() {
            sess.anchor = (next.0, next.1);
            sess.cur_len = next.2;
            sess.cur_text = self.span_text(next.0, next.1, next.2);
            self.snippet = Some(sess);
        }
        true
    }

    /// `len` chars of row `row` from char column `col`, clamped to the line.
    pub(super) fn span_text(&self, row: usize, col: usize, len: usize) -> String {
        self.lines
            .get(row)
            .map(|l| l.chars().skip(col).take(len).collect())
            .unwrap_or_default()
    }

    /// Move the caret to a resolved tab stop, selecting its placeholder text so
    /// the next keystroke replaces it.
    pub(super) fn place_at_snippet_stop(&mut self, stop: (usize, usize, usize)) {
        // A stop can outlive its row (a backspace join mid-session shrinks
        // the buffer): clamp to the live grid before planting anything —
        // clamping only the cursor left a selection whose row indexed past
        // the buffer end on the next edit.
        let (row, col, len) = stop;
        let row = row.min(self.lines.len().saturating_sub(1));
        let line_len = self.line_char_len(row);
        let col = col.min(line_len);
        let end = (col + len).min(line_len);
        self.cursor_row = row;
        self.cursor_col = end;
        self.selection = (end > col).then_some(EditorSelection {
            anchor: (row, col),
            head: (row, end),
        });
    }

    /// The Emmet dialect this buffer expands into, or `None` where the chord
    /// is not offered.
    pub(super) fn emmet_profile(&self) -> Option<crate::emmet::Profile> {
        emmet_profile_for(self.lang, self.path.as_deref())
    }

    /// Whether Emmet expansion is offered in this buffer at all, so the
    /// caller can tell "wrong language" apart from "nothing to expand".
    pub fn emmet_available(&self) -> bool {
        self.emmet_profile().is_some()
    }

    /// VS Code "Emmet: Expand Abbreviation"
    /// (`editor.emmet.action.expandAbbreviation`): replace the abbreviation
    /// ending at the cursor with the markup it stands for, and leave the
    /// caret in the first empty element.
    ///
    /// Only runs in markup buffers, and only when what sits left of the
    /// cursor actually parses as an abbreviation — so the chord is inert in
    /// prose and in code rather than mangling either. Returns whether
    /// anything expanded, which the caller reports in the status bar.
    pub fn expand_emmet_abbreviation(&mut self) -> bool {
        let Some(profile) = self.emmet_profile() else {
            return false;
        };
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = match self.lines.get(row) {
            Some(l) => l.clone(),
            None => return false,
        };
        let (start_col, abbr) = match crate::emmet::abbreviation_before(&line, col) {
            Some(v) => v,
            None => return false,
        };
        let unit = self.indent_unit();
        let (markup, caret) = match crate::emmet::expand(&abbr, &unit, profile) {
            Some(v) => v,
            None => return false,
        };

        // The abbreviation's own indentation prefixes every line of the
        // expansion, so an abbreviation typed inside a nested block stays
        // inside it.
        let lead: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let lead = if start_col == lead.chars().count() {
            lead
        } else {
            String::new()
        };

        self.pin_on_edit();
        self.push_undo(EditKind::EmmetExpand);
        self.last_edit_kind = None;

        let chars: Vec<char> = line.chars().collect();
        let before: String = chars[..start_col].iter().collect();
        let after: String = chars[col.min(chars.len())..].iter().collect();

        // `markup` always ends in a newline; the trailing line merges with
        // whatever followed the abbreviation.
        let body = markup.strip_suffix('\n').unwrap_or(&markup);
        let mut out: Vec<String> = Vec::new();
        let mut caret_row = row;
        let mut caret_col = 0usize;
        let mut consumed = 0usize;
        for (i, seg) in body.split('\n').enumerate() {
            let mut l = String::new();
            if i == 0 {
                l.push_str(&before);
            } else {
                l.push_str(&lead);
            }
            let text_start = l.chars().count();
            l.push_str(seg);
            // `caret` is a byte offset into `markup`; find the line holding
            // it and convert to a char column on the rebuilt line.
            let seg_end = consumed + seg.len();
            if caret >= consumed && caret <= seg_end {
                caret_row = row + i;
                caret_col = text_start + seg[..caret - consumed].chars().count();
            }
            consumed = seg_end + 1; // the '\n' we split on
            out.push(l);
        }
        if let Some(last) = out.last_mut() {
            last.push_str(&after);
        }

        self.lines.splice(row..=row, out);
        self.cursor_row = caret_row;
        self.cursor_col = caret_col;
        self.clear_selection();
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The buffer as one string, for the Emmet commands that edit markup
    /// already written (#1557), and `(row, col)` as a byte offset into it.
    pub(super) fn emmet_text_and_offset(&self, row: usize, col: usize) -> (String, usize) {
        let text = self.lines.join("\n");
        let before: usize = self.lines[..row.min(self.lines.len())]
            .iter()
            .map(|l| l.len() + 1)
            .sum();
        let line = self.lines.get(row).map_or("", String::as_str);
        let in_line = line.char_indices().nth(col).map_or(line.len(), |(i, _)| i);
        (text, before + in_line)
    }

    /// Byte offset `at` of `text` as a (row, char column).
    pub(super) fn emmet_position(text: &str, at: usize) -> (usize, usize) {
        let head = &text[..at.min(text.len())];
        let row = head.matches('\n').count();
        let start = head.rfind('\n').map_or(0, |i| i + 1);
        (row, head[start..].chars().count())
    }

    /// Make the buffer `text`, as one undo step, rewriting only the lines
    /// that changed.
    pub(super) fn emmet_set_text(&mut self, text: &str) {
        let new: Vec<String> = text.split('\n').map(String::from).collect();
        let (head, tail) = {
            let old = &self.lines;
            let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let room = old.len().min(new.len()) - head;
            let tail = old
                .iter()
                .rev()
                .zip(new.iter().rev())
                .take(room)
                .take_while(|(a, b)| a == b)
                .count();
            (head, tail)
        };
        self.pin_on_edit();
        self.push_undo(EditKind::EmmetExpand);
        self.last_edit_kind = None;
        let old_end = self.lines.len() - tail;
        let added = new.len() - tail - head;
        self.lines
            .splice(head..old_end, new[head..new.len() - tail].iter().cloned());
        // Rows below the rewrite keep their seats; the rewritten rows are
        // markup croft generated.
        self.provenance.splice(head, old_end - head, added);
        self.provenance
            .record(head..head + added, crate::provenance::Seat::Generated);
        self.provenance.truncate(self.lines.len());
        self.mark_buffer_changed();
        self.recompute_highlights();
    }

    /// Emmet: Wrap with Abbreviation (#1557): wrap the selected lines in
    /// `abbr`, one level in, at their own indentation; a selection inside
    /// one line is wrapped where it stands. With nothing selected it wraps
    /// the element around the caret, as VS Code does, or the caret's line
    /// outside any element. Returns whether anything was wrapped.
    pub fn emmet_wrap(&mut self, abbr: &str) -> bool {
        use crate::emmet::WrapContent;
        let Some(profile) = self.emmet_profile() else {
            return false;
        };
        let unit = self.indent_unit();
        let sel = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised())
            .or_else(|| self.emmet_element_at_caret());
        if let Some(((r0, c0), (r1, c1))) = sel
            && r0 == r1
        {
            let chars: Vec<char> = self.lines[r0].chars().collect();
            let before: String = chars[..c0].iter().collect();
            let after: String = chars[c1.min(chars.len())..].iter().collect();
            if !before.trim().is_empty() || !after.trim().is_empty() {
                let picked: String = chars[c0..c1.min(chars.len())].iter().collect();
                let Some(markup) =
                    crate::emmet::wrap(abbr, WrapContent::Inline(&picked), &unit, profile)
                else {
                    return false;
                };
                let markup = markup.trim_end_matches('\n');
                let (text, at) = self.emmet_text_and_offset(r0, c0);
                let end = at + picked.len();
                let new = format!("{}{markup}{}", &text[..at], &text[end..]);
                self.emmet_set_text(&new);
                (self.cursor_row, self.cursor_col) = Self::emmet_position(&new, at);
                self.clear_selection();
                self.ensure_cursor_col_visible();
                return true;
            }
        }
        let (first, last) = match sel {
            // A selection ending at the start of a line leaves that line out.
            Some(((r0, _), (r1, 0))) if r1 > r0 => (r0, r1 - 1),
            Some(((r0, _), (r1, _))) => (r0, r1),
            None => (self.cursor_row, self.cursor_row),
        };
        let rows = &self.lines[first..=last.min(self.lines.len() - 1)];
        if rows.iter().all(|l| l.trim().is_empty()) {
            return false;
        }
        // The lines' shared indentation stays outside the new element.
        let base: String = rows
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                l.chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
            })
            .min_by_key(String::len)
            .unwrap_or_default();
        let content: Vec<String> = rows
            .iter()
            .map(|l| {
                l.strip_prefix(base.as_str())
                    .unwrap_or(l.trim_start())
                    .to_string()
            })
            .collect();
        let Some(markup) = crate::emmet::wrap(abbr, WrapContent::Lines(&content), &unit, profile)
        else {
            return false;
        };
        let wrapped: Vec<String> = markup
            .trim_end_matches('\n')
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("{base}{l}")
                }
            })
            .collect();
        let mut lines = self.lines.clone();
        lines.splice(first..=last, wrapped);
        self.emmet_set_text(&lines.join("\n"));
        self.cursor_row = first;
        self.cursor_col = base.chars().count();
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// The span of the element around the caret, a caret just past its
    /// closing `>` included, for a wrap with nothing selected.
    pub(super) fn emmet_element_at_caret(&self) -> Option<((usize, usize), (usize, usize))> {
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let element = crate::emmet::enclosing(&text, at)
            .or_else(|| crate::emmet::enclosing(&text, at.checked_sub(1)?))?;
        let span = element.outer();
        Some((
            Self::emmet_position(&text, span.start),
            Self::emmet_position(&text, span.end),
        ))
    }

    /// Emmet: Remove Tag (#1557): take away the tags of the element around
    /// the caret, keeping its content.
    pub fn emmet_remove_tag(&mut self) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let unit = self.indent_unit();
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some((new, caret)) = crate::emmet::remove_tag(&text, at, &unit) else {
            return false;
        };
        self.emmet_set_text(&new);
        (self.cursor_row, self.cursor_col) = Self::emmet_position(&new, caret);
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// Emmet: Update Tag (#1557): rename the element around the caret.
    pub fn emmet_update_tag(&mut self, name: &str) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some(new) = crate::emmet::rename_tag(&text, at, name) else {
            return false;
        };
        self.emmet_set_text(&new);
        true
    }

    /// Emmet: Go to Matching Pair (#1557): from an opening tag to its
    /// closing tag, and back.
    pub fn emmet_match_tag(&mut self) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let (text, at) = self.emmet_text_and_offset(self.cursor_row, self.cursor_col);
        let Some(to) = crate::emmet::matching_tag(&text, at) else {
            return false;
        };
        (self.cursor_row, self.cursor_col) = Self::emmet_position(&text, to);
        self.clear_selection();
        self.ensure_cursor_col_visible();
        true
    }

    /// Emmet: Balance outward / inward (#1557): grow the selection to the
    /// enclosing element's content, then the element, and so on outward, or
    /// walk it back in.
    pub fn emmet_balance(&mut self, outward: bool) -> bool {
        if self.emmet_profile().is_none() {
            return false;
        }
        let ((r0, c0), (r1, c1)) = self.selection.map(|s| s.normalised()).unwrap_or((
            (self.cursor_row, self.cursor_col),
            (self.cursor_row, self.cursor_col),
        ));
        let (text, start) = self.emmet_text_and_offset(r0, c0);
        let end = self.emmet_text_and_offset(r1, c1).1;
        let range = if outward {
            crate::emmet::balance_out(&text, start..end)
        } else {
            crate::emmet::balance_in(&text, start..end)
        };
        let Some(range) = range else {
            return false;
        };
        let anchor = Self::emmet_position(&text, range.start);
        let head = Self::emmet_position(&text, range.end);
        self.selection = Some(EditorSelection { anchor, head });
        (self.cursor_row, self.cursor_col) = head;
        self.ensure_cursor_col_visible();
        true
    }
}

/// The Emmet dialect for a buffer, or `None` where expansion is not offered,
/// matching VS Code's default `emmet.includeLanguages`. `.jsx` shares
/// `LangKind::JavaScript` with plain `.js`, so the extension tells them
/// apart: an abbreviation in ordinary JavaScript is code, not markup.
pub(super) fn emmet_profile_for(
    lang: Option<LangKind>,
    path: Option<&std::path::Path>,
) -> Option<crate::emmet::Profile> {
    use crate::emmet::Profile;
    match lang {
        Some(LangKind::Html | LangKind::Vue | LangKind::Svelte) => Some(Profile::Html),
        Some(LangKind::Xml) => Some(Profile::Xml),
        Some(LangKind::Tsx) => Some(Profile::Jsx),
        Some(LangKind::JavaScript)
            if path
                .and_then(|p| p.extension())
                .is_some_and(|e| e.eq_ignore_ascii_case("jsx")) =>
        {
            Some(Profile::Jsx)
        }
        _ => None,
    }
}
