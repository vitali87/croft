//! The panel's input fields: the caret and selection in each, typing and deleting, which
//! field has focus, the Replace and details rows shown or hidden, and the include and
//! exclude filter the fields make.

use super::*;

impl SearchPanel {
    /// Screen position of the focused field's text caret, in absolute terminal
    /// coordinates, for the app to drive the host's blinking hardware caret
    /// (mirroring `SourceControlPanel::cursor_screen_pos`). `None` when the
    /// panel isn't focused or hasn't been laid out yet.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }
        self.caret_cell
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    /// Replace the query and both filter lists wholesale (the terminal-seed
    /// path). Text replacement invalidates any byte-range selection into the
    /// old text — a stale range into shorter text panics on the next edit —
    /// and focus moves to the query the caller just filled.
    pub fn seed(&mut self, query: String, include: String, exclude: String) {
        self.query = query;
        self.include = include;
        self.exclude = exclude;
        self.field = SearchField::Query;
        self.selection = None;
        self.field_selection = None;
        for f in [
            SearchField::Query,
            SearchField::Include,
            SearchField::Exclude,
        ] {
            self.carets[f.index()] = None;
        }
    }

    /// The focused field's caret as a char index into its text.
    pub fn caret(&self) -> usize {
        self.field_caret(self.field)
    }

    pub(super) fn field_text(&self, field: SearchField) -> &str {
        match field {
            SearchField::Query => &self.query,
            SearchField::Replace => &self.replace,
            SearchField::Include => &self.include,
            SearchField::Exclude => &self.exclude,
        }
    }

    pub(super) fn field_caret(&self, field: SearchField) -> usize {
        let text = self.field_text(field);
        crate::widgets::line_edit::clamp(text, self.carets[field.index()].unwrap_or(usize::MAX))
    }

    /// Record `at` as `field`'s caret; the end of the text is stored as
    /// "follow the end".
    pub(super) fn store_caret(&mut self, field: SearchField, at: usize) {
        let len = crate::widgets::line_edit::char_len(self.field_text(field));
        self.carets[field.index()] = (at < len).then_some(at);
    }

    pub(super) fn set_caret(&mut self, at: usize) {
        self.store_caret(self.field, at);
    }

    /// Move the focused field's caret with `step(text, caret)`, dropping the
    /// field's selection as any caret key does.
    pub fn move_caret(&mut self, step: impl FnOnce(&str, usize) -> usize) {
        let at = step(self.active_text(), self.caret());
        self.set_active_selection(None);
        self.set_caret(at);
    }

    /// Put the focused field's caret under screen column `col`, from a click
    /// in it (the text origin is the one the last render captured).
    pub fn click_caret(&mut self, col: u16) {
        let (text_x, start) = self.field_origin[self.field.index()];
        let at = crate::widgets::line_edit::char_at_cell(
            self.active_text(),
            start,
            usize::from(col.saturating_sub(text_x)),
        );
        self.set_active_selection(None);
        self.set_caret(at);
    }

    /// Apply `edit(text, caret) -> (text, caret)` to the focused field.
    pub(super) fn edit_active(
        &mut self,
        edit: impl FnOnce(&str, usize) -> (String, usize),
    ) -> bool {
        let (text, caret) = edit(self.active_text(), self.caret());
        let changed = text != self.active_text();
        *self.active_text_mut() = text;
        self.set_caret(caret);
        changed
    }

    pub fn select_all_query(&mut self) {
        if self.query.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((0, self.query.len()));
        }
        self.carets[SearchField::Query.index()] = None;
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection_range() else {
            return false;
        };
        if a == b {
            self.selection = None;
            return false;
        }
        self.query.replace_range(a..b, "");
        self.selection = None;
        let at = crate::widgets::line_edit::char_of(&self.query, a);
        self.store_caret(SearchField::Query, at);
        true
    }

    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) if a < b => self.query[a..b].to_string(),
            _ => String::new(),
        }
    }

    /// Mutable handle to the text of whichever field is focused. The Query
    /// field keeps its own `selection`; the other fields share
    /// `field_selection`, so selecting text in one input clears it elsewhere.
    pub(super) fn active_text_mut(&mut self) -> &mut String {
        match self.field {
            SearchField::Query => &mut self.query,
            SearchField::Replace => &mut self.replace,
            SearchField::Include => &mut self.include,
            SearchField::Exclude => &mut self.exclude,
        }
    }

    /// Read-only view of the focused field's text.
    pub fn active_text(&self) -> &str {
        match self.field {
            SearchField::Query => &self.query,
            SearchField::Replace => &self.replace,
            SearchField::Include => &self.include,
            SearchField::Exclude => &self.exclude,
        }
    }

    /// Clear the focused field's text entirely (used by Esc).
    pub fn active_text_mut_clear(&mut self) {
        self.active_text_mut().clear();
        self.set_caret(0);
    }

    /// Clear the selection of the focused field.
    pub fn clear_active_selection(&mut self) {
        self.set_active_selection(None);
    }

    pub(super) fn active_selection(&self) -> Option<(usize, usize)> {
        let sel = match self.field {
            SearchField::Query => self.selection,
            _ => self.field_selection,
        };
        sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    pub(super) fn set_active_selection(&mut self, sel: Option<(usize, usize)>) {
        match self.field {
            SearchField::Query => self.selection = sel,
            _ => self.field_selection = sel,
        }
    }

    /// Append `c` to the focused field, replacing any selection first.
    pub fn type_char(&mut self, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        self.delete_active_selection();
        let typed = c.to_string();
        self.edit_active(|t, at| crate::widgets::line_edit::insert_at(t, at, &typed));
    }

    /// Backspace one char (or the selection) from the focused field. Returns
    /// true when the text changed.
    pub fn backspace(&mut self) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_before(t, at, false))
    }

    /// Delete the cluster (or, with `word`, the run) before the caret, or the
    /// selection. Returns true when the text changed.
    pub fn backspace_by(&mut self, word: bool) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_before(t, at, word))
    }

    /// Delete ahead of the caret (the Delete key), or the selection.
    /// Returns true when the text changed.
    pub fn delete_forward(&mut self, word: bool) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_at(t, at, word))
    }

    /// Insert `s` into the focused field, replacing the selection and stripping
    /// newlines (all Search inputs are single-line).
    pub fn insert_str(&mut self, s: &str) {
        self.delete_active_selection();
        self.edit_active(|t, at| crate::widgets::line_edit::insert_at(t, at, s));
    }

    /// Select the whole text of the focused field.
    pub fn select_all_active(&mut self) {
        let len = self.active_text().len();
        if len == 0 {
            self.set_active_selection(None);
        } else {
            self.set_active_selection(Some((0, len)));
        }
        let end = crate::widgets::line_edit::char_len(self.active_text());
        self.set_caret(end);
    }

    pub(super) fn delete_active_selection(&mut self) -> bool {
        let Some((a, b)) = self.active_selection() else {
            return false;
        };
        if a == b {
            self.set_active_selection(None);
            return false;
        }
        self.active_text_mut().replace_range(a..b, "");
        self.set_active_selection(None);
        let at = crate::widgets::line_edit::char_of(self.active_text(), a);
        self.set_caret(at);
        true
    }

    /// Text currently selected in the focused field (empty when none).
    pub fn active_selection_text(&self) -> String {
        match self.active_selection() {
            Some((a, b)) if a < b => self.active_text()[a..b].to_string(),
            _ => String::new(),
        }
    }

    /// Move keyboard focus to the next visible field, wrapping around. Only
    /// fields whose row is currently expanded participate, mirroring VS Code
    /// where Tab skips collapsed inputs.
    pub fn focus_next_field(&mut self) {
        let order = self.visible_fields();
        let pos = order.iter().position(|f| *f == self.field).unwrap_or(0);
        let next = order[(pos + 1) % order.len()];
        self.focus_field(next);
    }

    /// Set the focused field, clearing selection in the field being left.
    pub fn focus_field(&mut self, field: SearchField) {
        if self.field != field {
            self.set_active_selection(None);
            self.field = field;
        }
    }

    /// Visible (focusable) fields, in tab order, honouring which rows are
    /// expanded. Query is always present.
    pub(super) fn visible_fields(&self) -> Vec<SearchField> {
        let mut v = vec![SearchField::Query];
        if self.replace_open {
            v.push(SearchField::Replace);
        }
        if self.details_open {
            v.push(SearchField::Include);
            v.push(SearchField::Exclude);
        }
        v
    }

    /// Toggle the Replace row. When collapsing while it holds focus, focus
    /// falls back to the Query field.
    pub fn toggle_replace(&mut self) {
        self.replace_open = !self.replace_open;
        if self.replace_open {
            // VS Code focuses the revealed input; leaving focus in the
            // query made the next keystrokes silently extend the search
            // (#123).
            self.focus_field(SearchField::Replace);
        } else if self.field == SearchField::Replace {
            self.focus_field(SearchField::Query);
        }
    }

    /// Expand the Replace row if it is collapsed and focus it: VS Code's
    /// Replace in Files (#860). Unlike the chevron, it never collapses.
    pub fn open_replace(&mut self) {
        self.replace_open = true;
        self.focus_field(SearchField::Replace);
    }

    /// Toggle the include/exclude detail rows. Collapsing while one holds
    /// focus returns focus to the Query field.
    pub fn toggle_details(&mut self) {
        self.details_open = !self.details_open;
        if !self.details_open && matches!(self.field, SearchField::Include | SearchField::Exclude) {
            self.focus_field(SearchField::Query);
        }
    }

    /// Compile the current include/exclude inputs into a `PathFilter`.
    pub fn path_filter(&self) -> PathFilter {
        PathFilter::new(&self.include, &self.effective_exclude())
    }

    /// The exclude box plus, while `use_exclude_settings` is on, the
    /// project's exclusion globs (#1345): what the next scan leaves out.
    pub fn effective_exclude(&self) -> String {
        if !self.use_exclude_settings || self.project_exclude.is_empty() {
            return self.exclude.clone();
        }
        let mut parts: Vec<&str> = Vec::new();
        if !self.exclude.trim().is_empty() {
            parts.push(&self.exclude);
        }
        parts.extend(self.project_exclude.iter().map(String::as_str));
        parts.join(", ")
    }
}
