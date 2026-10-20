//! A single-line text input modal for Source Control operations that need
//! a typed value: a clone URL, a new branch name, a remote name/URL, a tag
//! name. Croft's design rule keeps inputs in a visible popup (never the
//! status line), so every such prompt routes through this widget.
//!
//! The widget is pure: it owns the prompt text, the typed value, and the
//! caret. The App holds it inside `Option<InputPrompt>` with a `purpose`
//! tag so a single key/mouse/render path serves every prompt, and decides
//! what to do with the submitted string.
//!
//! A yes/no question ([`InputPrompt::confirm`]) uses the same box with no
//! field, so nothing typed can look like an answer the App then ignores.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// The four ways VS Code's Databases view adds a database (#578).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeqlDbSource {
    Folder,
    Archive,
    Url,
    Github,
}

/// Why an input prompt is open, so the App can route the submitted value
/// to the right git operation. Variants carry whatever context the second
/// step needs (e.g. the base ref a new branch forks from).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputPurpose {
    /// Extract one archive member (#179): the typed value is the
    /// destination folder.
    ArchiveExtract {
        member: String,
    },
    /// Name a new worktree lane (#348): the typed value becomes the branch
    /// `agent/<slug>` and a sibling directory beside the repo.
    NewWorktreeLane,
    /// The command to run across the fleet (#363).
    FleetCommand,
    /// The host to push the syncable config to now (#262).
    SyncConfigHost,
    /// The pull request to review (#365): a number, `#n`, or its URL.
    PullRequestNumber,
    /// Where to add a CodeQL database from (#578).
    CodeqlDatabase {
        source: CodeqlDbSource,
    },
    /// Confirm removing the CodeQL database at `path` (#578). Submitting
    /// (Enter) removes it; Esc keeps it. A Y/N confirm, with no value.
    CodeqlRemoveDatabase {
        path: PathBuf,
    },
    /// Confirm deleting the unused CodeQL databases at `paths` (#578), all
    /// copies in croft's cache. Submitting (Enter) deletes them; Esc keeps
    /// them. A Y/N confirm, with no value.
    CodeqlDeleteUnusedDatabases {
        paths: Vec<PathBuf>,
    },
    /// A new display name for the CodeQL database at `path` (#578).
    CodeqlRenameDatabase {
        path: PathBuf,
    },
    /// Confirm removing the query history entry whose run wrote `output`
    /// (#578). Submitting (Enter) removes it; Esc keeps it. A Y/N confirm,
    /// with no value.
    CodeqlRemoveHistory {
        output: PathBuf,
    },
    /// A label for the query history entry whose run wrote `output` (#578).
    CodeqlRenameHistory {
        output: PathBuf,
    },
    /// The name of a new CodeQL query to write into `dir` (#578), in
    /// `language` when it is known.
    CodeqlCreateQuery {
        dir: PathBuf,
        language: Option<String>,
    },
    /// The controller repository variant analysis runs from (#578):
    /// `owner/repo` or its GitHub URL.
    CodeqlControllerRepository,
    /// A repository to add for variant analysis (#578), into the list
    /// called `list` when there is one.
    CodeqlAddVariantRepo {
        list: Option<String>,
    },
    /// The name of a new variant analysis repository list (#578).
    CodeqlAddVariantList,
    /// A GitHub user or organisation to add for variant analysis (#578).
    CodeqlAddVariantOwner,
    /// The CodeQL packs to download from the registry (#578).
    CodeqlDownloadPacks,
    /// Where to copy the query results at `output` (#578).
    CodeqlExportResults {
        output: PathBuf,
    },
    /// CodeQL: Run Queries in Published Pack (#578): the pack reference.
    CodeqlRunPublishedPack,
    /// A GitHub Code Search query whose repositories go into the variant
    /// analysis list called `list` (#578).
    CodeqlVariantCodeSearch {
        list: String,
    },
    /// Where to export the results of remembered variant analysis `index`
    /// (#578): a folder, or `gist`.
    CodeqlExportVariantResults {
        index: usize,
    },
    /// A new name for the variant analysis list called `name` (#578).
    CodeqlRenameVariantList {
        name: String,
    },
    /// Confirm removing the variant analysis list called `name` (#578).
    /// Submitting (Enter) removes it; Esc keeps it. A Y/N confirm, with no
    /// value.
    CodeqlRemoveVariantList {
        name: String,
    },
    /// Where a SARIF result's file lives on this machine (#577): the value
    /// is a path; `uri` is the location the log named.
    SarifLocate {
        uri: String,
    },
    /// The kind of Model Editor model `choice` of endpoint `endpoint`
    /// (#578): `remote`, `command-injection`, `taint`, …
    CodeqlModelKind {
        endpoint: usize,
        choice: usize,
    },
    /// A missing SARIF file that can be downloaded from `host`, which is
    /// not yet trusted (#577). An empty value trusts `host` and downloads;
    /// a path locates the file instead, as [`InputPurpose::SarifLocate`].
    SarifTrustHost {
        uri: String,
        host: String,
        url: String,
    },
    /// Another SARIF log to merge into the open viewer (#577).
    SarifAddLog,
    /// The SARIF results list's optional columns, comma-separated (#577).
    SarifColumns,
    /// A baseline SARIF log to compare the open viewer against (#577).
    SarifBaseline,
    /// Where to write the SARIF viewer's visible results as CSV (#577).
    SarifExport,
    /// The optional comment for dismissing code scanning alert `number`
    /// with reason `reason` (an index into `DismissReason::ALL`).
    DismissAlertComment {
        number: u64,
        reason: usize,
    },
    /// A new value for the setting `key` (#612).
    SettingValue {
        key: String,
    },
    /// Find in a hex tab (#172): the typed value is hex byte pairs
    /// ("de ad be ef") or, when it does not parse as hex, literal ASCII.
    /// Submitting stores the query on the view and jumps to the first
    /// match at or after the cursor; F3 repeats it.
    HexFind,
    /// Emmet: Wrap with Abbreviation (#1557): the typed abbreviation wraps
    /// the selection.
    EmmetWrap,
    /// Emmet: Update Tag (#1557): the typed name replaces the enclosing
    /// element's.
    EmmetUpdateTag,
    /// Save the current folder set as a VS Code-compatible workspace
    /// file (#163): the typed value is the destination path.
    SaveWorkspaceAs,
    CloneUrl,
    RenameBranch,
    CreateBranchFrom {
        base: String,
    },
    AddRemoteName,
    AddRemoteUrl {
        name: String,
    },
    CreateTag,
    /// Create Tag Here on a COMMITS row (#1627): the tag goes on `target`,
    /// the clicked commit, rather than on HEAD.
    CreateTagAt {
        target: String,
    },
    /// The optional message for tag `name` (#1582): one makes an annotated
    /// tag, none (Enter on a blank field) a lightweight one.
    CreateTagMessage {
        name: String,
    },
    /// Confirm deleting tag `name` from `remote` (#1582). Submitting (Enter)
    /// deletes it there; Esc keeps it. A Y/N confirm, with no value. `root` is
    /// the repository whose remote was listed: the deletion is dropped if
    /// Source Control has moved to another one by then.
    DeleteRemoteTag {
        root: std::path::PathBuf,
        remote: String,
        name: String,
    },
    /// Add a debugger watch expression (#112): submitting appends it to the
    /// App's watch list and re-evaluates at the current stop.
    AddWatch,
    /// First-run consent for an extension-contributed viewer (#485 review):
    /// the same gate a sidecar passes, since both spawn a program from a
    /// manifest. Carries the viewer's key and the file, so allowing resumes
    /// the open the user asked for.
    ViewerConsent {
        key: String,
        path: std::path::PathBuf,
    },
    /// First-run consent to spawn an MCP sidecar: submitting confirms, then the
    /// command (`command_id`) proceeds to its argument prompt or runs.
    McpConsent {
        command_id: String,
    },
    /// Collect the single string argument for an MCP command before calling its
    /// tool. The submitted value fills the tool's declared argument.
    McpArg {
        command_id: String,
    },
    /// Collect a repeat count for "Macro: Replay N Times…" (#255). A value
    /// that is not a positive number is refused rather than silently
    /// replaying once.
    MacroReplayCount,
    /// The optional name for a stash (#1581), as VS Code's Git: Stash asks:
    /// one becomes `git stash push -m`, a blank field keeps git's own
    /// `WIP on <branch>: <commit>`.
    StashMessage {
        kind: crate::git::StashKind,
    },
    /// Confirm uninstalling an installed extension. Submitting (Enter) performs
    /// the removal; Esc keeps it. A Y/N confirm, with no value.
    ExtensionUninstall {
        id: String,
    },
    /// Confirm moving Explorer file-tree entries to the OS Trash. Submitting
    /// (Enter) trashes `paths`; Esc keeps them. A Y/N confirm, with no value:
    /// a prefilled `trash` read as type-to-confirm, yet any answer trashed
    /// (#855).
    TreeDelete {
        paths: Vec<PathBuf>,
    },
    /// A dirty buffer collided with an external write. Submitting (Enter)
    /// reloads `paths` from disk, discarding local edits; Esc keeps the edits.
    /// The value is a sentinel.
    ReloadConflict {
        paths: Vec<PathBuf>,
    },
    /// Instruction for the resident navigator (`croft pair`), scoped to the
    /// 0-based inclusive line `range` of `file`; `selection` carries the
    /// selected text when the scope came from a selection. Submitting sends
    /// the ask turn (the navigator may edit on it).
    AskNavigator {
        file: String,
        range: (usize, usize),
        selection: String,
    },
    /// One field of **Debug: Add Configuration…** (#250); the draft it
    /// fills is the App's.
    DebugConfigField {
        field: crate::dap::configs::DraftField,
    },
    /// A `${input:…}` a launch or task asks for (#1424); the question
    /// itself is the App's.
    LaunchInput,
}

pub struct InputPrompt {
    pub purpose: InputPurpose,
    pub title: String,
    pub placeholder: String,
    pub value: String,
    pub cursor: usize,
    pub last_rect: Rect,
    /// Whether Enter on an empty field submits (an optional field left
    /// blank) rather than waiting for a value.
    pub allow_blank: bool,
    /// The line under the field; `None` is the generic "Enter to confirm ·
    /// Esc to cancel".
    pub hint: Option<String>,
    /// Draw the value as dots (a `"password": true` input, #1424).
    pub masked: bool,
    /// The whole value is selected (#1601): a prefilled value opens this
    /// way, as VS Code's input boxes do, so typing or pasting replaces it
    /// and Backspace clears it, where before every key appended to it.
    pub selected: bool,
    /// A yes/no question with no field (#855): Enter or Y answers yes, Esc
    /// or N no, and typing goes nowhere. A sentinel value in an editable
    /// field read as type-to-confirm, yet `no` + Enter still went ahead.
    pub confirm: bool,
}

impl InputPrompt {
    pub fn new(
        purpose: InputPurpose,
        title: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self {
            purpose,
            title: title.into(),
            placeholder: placeholder.into(),
            value: String::new(),
            cursor: 0,
            last_rect: Rect::default(),
            allow_blank: false,
            hint: None,
            masked: false,
            selected: false,
            confirm: false,
        }
    }

    /// A yes/no question (#855): `title` asks it and `hint` says which keys
    /// answer it. Submitting carries an empty value.
    pub fn confirm(
        purpose: InputPurpose,
        title: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            hint: Some(hint.into()),
            confirm: true,
            ..Self::new(purpose, title, "")
        }
    }

    /// Say under the field what Enter and Esc do, when it is more than
    /// confirm and cancel.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Draw the typed value as dots.
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Let Enter submit an empty field, for a value that is optional.
    pub fn allowing_blank(mut self) -> Self {
        self.allow_blank = true;
        self
    }

    /// Seed the field with an initial value (e.g. the current branch name
    /// for a rename), caret at the end.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.chars().count();
        self.selected = !self.value.is_empty();
        self
    }

    /// Select the whole value (Ctrl+A), caret at its end.
    pub fn select_all(&mut self) {
        self.cursor = self.char_count();
        self.selected = !self.value.is_empty();
    }

    /// Drop a selected value, for the key about to replace or delete it.
    /// True when there was one.
    fn take_selection(&mut self) -> bool {
        if !std::mem::take(&mut self.selected) {
            return false;
        }
        self.value.clear();
        self.cursor = 0;
        true
    }

    /// Delete from the start of the value to the caret (Ctrl+U).
    pub fn clear_to_start(&mut self) {
        if self.take_selection() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.value.replace_range(..at, "");
        self.cursor = 0;
    }

    /// Delete from the caret to the end of the value (Ctrl+K).
    pub fn clear_to_end(&mut self) {
        if self.take_selection() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.value.truncate(at);
    }

    /// Delete the word before the caret (Ctrl+W, Alt/Ctrl+Backspace): the
    /// separators after it, then back to the previous space or path
    /// separator, so `/a/b/c.txt` goes to `/a/b/`.
    pub fn delete_word_back(&mut self) {
        if self.take_selection() {
            return;
        }
        let chars: Vec<char> = self.value.chars().collect();
        let is_sep = |c: char| c.is_whitespace() || c == '/' || c == '\\';
        let mut start = self.cursor.min(chars.len());
        while start > 0 && is_sep(chars[start - 1]) {
            start -= 1;
        }
        while start > 0 && !is_sep(chars[start - 1]) {
            start -= 1;
        }
        let (from, to) = (self.byte_offset(start), self.byte_offset(self.cursor));
        self.value.replace_range(from..to, "");
        self.cursor = start;
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    /// Type `c` at the caret; a confirm has no field, so it takes none.
    pub fn push_char(&mut self, c: char) {
        if self.confirm || c == '\n' || c == '\r' {
            return;
        }
        self.take_selection();
        let at = self.byte_offset(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    pub fn pop_char(&mut self) {
        if self.take_selection() || self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.value.remove(at);
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.take_selection() || self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.value.remove(at);
    }

    /// Left on a selected value goes to its start, as in a text field.
    pub fn move_cursor_left(&mut self) {
        self.cursor = match std::mem::take(&mut self.selected) {
            true => 0,
            false => self.cursor.saturating_sub(1),
        };
    }

    /// Right on a selected value goes to its end.
    pub fn move_cursor_right(&mut self) {
        self.cursor = match std::mem::take(&mut self.selected) {
            true => self.char_count(),
            false => (self.cursor + 1).min(self.char_count()),
        };
    }

    pub fn move_cursor_home(&mut self) {
        self.selected = false;
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.selected = false;
        self.cursor = self.char_count();
    }

    /// The trimmed submitted value, or `None` when blank (Enter on an empty
    /// field is a no-op the caller treats as "keep waiting"). A confirm's
    /// answer is always the empty value.
    pub fn submit_value(&self) -> Option<String> {
        let v = self.value.trim();
        (self.allow_blank || self.confirm || !v.is_empty()).then(|| v.to_string())
    }
}

/// Where a field `width` cells wide starts showing `value` so the caret
/// at char `cursor` is in view (#1601): the chars skipped, and whether a
/// leading `…` takes the first cell to say so. Measured in terminal cells,
/// so wide (CJK) chars count as two.
fn field_scroll(value: &str, cursor: usize, width: usize) -> (usize, bool) {
    use unicode_width::UnicodeWidthChar;
    let cells: Vec<usize> = value.chars().map(|c| c.width().unwrap_or(0)).collect();
    let cursor = cursor.min(cells.len());
    // The caret's own cell: the char under it, or one blank past the end.
    let caret = cells.get(cursor).copied().unwrap_or(1).max(1);
    let before: usize = cells[..cursor].iter().sum();
    if width < 2 || before + caret <= width {
        return (0, false);
    }
    // `…`, then as many chars before the caret as fit, then the caret.
    let room = width.saturating_sub(1 + caret);
    let (mut skip, mut used) = (cursor, 0);
    while skip > 0 && used + cells[skip - 1] <= room {
        skip -= 1;
        used += cells[skip];
    }
    (skip, true)
}

pub fn render_input_prompt(
    prompt: &mut InputPrompt,
    screen: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    let width = (screen.width.saturating_mul(6) / 10)
        .clamp(30, 90)
        .min(screen.width);
    // A confirm is the title and one line of keys: it has no field.
    let height: u16 = if prompt.confirm { 3 } else { 5 };
    let rect = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 3,
        width,
        height: height.min(screen.height),
    };
    prompt.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        format!(" {} ", prompt.title),
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f))));
    let inner = Rect {
        x: rect.x + 2,
        y: rect.y + 1,
        width: rect.width.saturating_sub(4),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if theme.gradient() {
        crate::gradient::paint_gradient_box(buf, rect);
        // Clipped to the box like the block's own title: at full width it
        // ran past the border and the screen edge (#910).
        buf.set_span(rect.x + 1, rect.y, &title, rect.width.saturating_sub(2));
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if prompt.confirm {
        buf.set_stringn(
            inner.x,
            inner.y,
            prompt.hint.as_deref().unwrap_or_default(),
            inner.width as usize,
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4))),
        );
        return;
    }

    // Value row with a blinking caret, or the placeholder when empty.
    let value_y = inner.y;
    if prompt.value.is_empty() {
        buf.set_stringn(
            inner.x,
            value_y,
            &prompt.placeholder,
            inner.width as usize,
            Style::default()
                .fg(theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)))
                .add_modifier(Modifier::ITALIC),
        );
    } else {
        let shown = if prompt.masked {
            "•".repeat(prompt.value.chars().count())
        } else {
            prompt.value.clone()
        };
        let cursor = prompt.cursor.min(shown.chars().count());
        // The field scrolls to keep the caret in view (#1601): a value wider
        // than the box, any real path, hid its end and the caret, which
        // starts there. A `…` marks the hidden start.
        let (skip, lead) = field_scroll(&shown, cursor, inner.width as usize);
        let before: String = shown.chars().skip(skip).take(cursor - skip).collect();
        let at: String = shown.chars().skip(cursor).take(1).collect();
        let after: String = shown.chars().skip(cursor + 1).collect();
        let caret = if at.is_empty() { " ".to_string() } else { at };
        let mut value_style = Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)));
        if prompt.selected {
            value_style = value_style.bg(theme.ui(Color::Rgb(0x26, 0x4f, 0x78)));
        }
        let caret_style = Style::default()
            .fg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f)))
            .bg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
            .add_modifier(Modifier::SLOW_BLINK);
        let dim = Style::default().fg(theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)));
        let line = Line::from(vec![
            Span::styled(if lead { "\u{2026}" } else { "" }, dim),
            Span::styled(before, value_style),
            Span::styled(caret, caret_style),
            Span::styled(after, value_style),
        ]);
        Widget::render(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y: value_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
    }

    if inner.height >= 3 {
        buf.set_stringn(
            inner.x,
            inner.y + 2,
            prompt
                .hint
                .as_deref()
                .unwrap_or("Enter to confirm · Esc to cancel"),
            inner.width as usize,
            Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_title_stays_inside_the_box_under_the_gradient_theme() {
        // #910: the gradient repaint drew the title at full width, past the
        // border and the screen edge, taking "unsaved edits" with it.
        let mut p = InputPrompt::new(
            InputPurpose::CreateTag,
            format!(
                "{} changed on disk and you have unsaved edits.",
                "x/".repeat(60)
            ),
            "",
        );
        let screen = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(screen);
        render_input_prompt(&mut p, screen, &mut buf, crate::theme::Theme::BLACK);
        let r = p.last_rect;
        for x in r.x + r.width..screen.width {
            assert_eq!(buf[(x, r.y)].symbol(), " ", "column {x} of the title row");
        }
    }

    fn prefilled(value: &str) -> InputPrompt {
        InputPrompt::new(InputPurpose::CreateTag, "Tag", "name").with_value(value)
    }

    /// #1601: a prefilled value opens selected, so typing replaces it.
    #[test]
    fn typing_over_a_prefilled_value_replaces_it() {
        let mut p = prefilled("abc");
        assert!(p.selected);
        p.push_char('x');
        p.push_char('y');
        assert_eq!((p.value.as_str(), p.cursor), ("xy", 2));
    }

    /// #1601: Backspace or Delete on a selected value clears all of it.
    #[test]
    fn backspace_or_delete_clears_a_selected_value() {
        let mut p = prefilled("abc");
        p.pop_char();
        assert_eq!(p.value, "");
        let mut p = prefilled("abc");
        p.delete_char();
        assert_eq!(p.value, "");
    }

    /// #1601: Ctrl+U clears to the start and Ctrl+K to the end.
    #[test]
    fn clearing_to_the_start_or_the_end_of_the_value() {
        let mut p = prefilled("abcdef");
        p.move_cursor_left();
        for _ in 0..3 {
            p.move_cursor_right();
        }
        p.clear_to_start();
        assert_eq!((p.value.as_str(), p.cursor), ("def", 0));
        p.move_cursor_right();
        p.clear_to_end();
        assert_eq!((p.value.as_str(), p.cursor), ("d", 1));
        let mut p = prefilled("abc");
        p.clear_to_start();
        assert_eq!(p.value, "", "a selected value goes whole");
    }

    /// #1601: a word delete stops at a path separator.
    #[test]
    fn a_word_delete_goes_back_to_the_path_separator() {
        let mut p = prefilled("/a/b/c.txt");
        p.move_cursor_end();
        p.delete_word_back();
        assert_eq!(p.value, "/a/b/");
        p.delete_word_back();
        assert_eq!(p.value, "/a/");
        let mut p = prefilled("git tag v1");
        p.move_cursor_end();
        p.delete_word_back();
        assert_eq!(p.value, "git tag ");
    }

    /// #1601: Ctrl+A selects the value again after it was deselected.
    #[test]
    fn select_all_selects_the_value_again() {
        let mut p = prefilled("abc");
        p.move_cursor_end();
        assert!(!p.selected);
        p.select_all();
        p.push_char('z');
        assert_eq!(p.value, "z");
    }

    /// Negative (#1601): an arrow key deselects and keeps the value, so
    /// typing then edits it in place.
    #[test]
    fn an_arrow_key_keeps_a_prefilled_value_to_edit() {
        let mut p = prefilled("abc");
        p.move_cursor_left();
        p.push_char('x');
        assert_eq!(p.value, "xabc");
        let mut p = prefilled("abc");
        p.move_cursor_end();
        p.push_char('d');
        assert_eq!(p.value, "abcd");
    }

    /// Negative (#1601): an empty prompt is not selected; typing builds it.
    #[test]
    fn an_empty_prompt_types_as_before() {
        let mut p = InputPrompt::new(InputPurpose::CreateTag, "Tag", "name").with_value("");
        assert!(!p.selected);
        p.push_char('a');
        p.push_char('b');
        p.pop_char();
        assert_eq!(p.value, "a");
    }

    fn value_row(p: &mut InputPrompt, width: u16) -> String {
        let screen = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(screen);
        render_input_prompt(p, screen, &mut buf, crate::theme::Theme::BLACK);
        let r = p.last_rect;
        (r.x + 2..r.x + r.width - 2)
            .map(|x| buf[(x, r.y + 1)].symbol().to_string())
            .collect()
    }

    /// #1601: a value wider than the field scrolls to keep the caret, at
    /// its end, in view, with a `…` for the hidden start.
    #[test]
    fn a_long_value_scrolls_to_keep_the_caret_in_view() {
        let long = format!("/work/{}/end.code-workspace", "dir/".repeat(40));
        let mut p = prefilled(&long);
        let row = value_row(&mut p, 50);
        assert!(row.starts_with('\u{2026}'), "{row}");
        assert!(row.trim_end().ends_with("end.code-workspace"), "{row}");
        p.move_cursor_home();
        let row = value_row(&mut p, 50);
        assert!(row.starts_with("/work/dir/"), "{row}");
    }

    /// #1601: scrolling is measured in cells, so a value of wide chars
    /// that is fewer chars than the field but more cells still scrolls and
    /// keeps the end caret inside the field.
    #[test]
    fn a_wide_char_value_scrolls_by_display_width() {
        let mut p = prefilled(&"\u{4e2d}".repeat(20));
        let screen = Rect::new(0, 0, 50, 12);
        let mut buf = Buffer::empty(screen);
        render_input_prompt(&mut p, screen, &mut buf, crate::theme::Theme::BLACK);
        let r = p.last_rect;
        let (left, right) = (r.x + 2, r.x + r.width - 2);
        assert!(
            20 < right - left && right - left < 40,
            "field is {}",
            right - left
        );
        let caret = (left..right)
            .find(|&x| buf[(x, r.y + 1)].modifier.contains(Modifier::SLOW_BLINK))
            .expect("caret drawn inside the field");
        assert!(caret < right);
        assert_eq!(buf[(left, r.y + 1)].symbol(), "\u{2026}");
        p.move_cursor_home();
        assert!(value_row(&mut p, 50).starts_with('\u{4e2d}'));
    }

    /// Negative (#1601): a value that fits is drawn from its first char.
    #[test]
    fn a_short_value_is_drawn_whole() {
        let mut p = prefilled("v1.2.3");
        let row = value_row(&mut p, 50);
        assert!(row.starts_with("v1.2.3"), "{row}");
    }

    #[test]
    fn submit_value_trims_and_rejects_blank() {
        let mut p = InputPrompt::new(InputPurpose::CreateTag, "Tag", "name");
        assert_eq!(p.submit_value(), None);
        for c in "  v1  ".chars() {
            p.push_char(c);
        }
        assert_eq!(p.submit_value().as_deref(), Some("v1"));
    }

    #[test]
    fn with_value_seeds_text_and_caret_at_end() {
        let p =
            InputPrompt::new(InputPurpose::RenameBranch, "Rename", "new name").with_value("main");
        assert_eq!(p.value, "main");
        assert_eq!(p.cursor, 4);
    }

    #[test]
    fn cursor_edits_mid_string() {
        let mut p = InputPrompt::new(InputPurpose::CloneUrl, "Clone", "url");
        for c in "abc".chars() {
            p.push_char(c);
        }
        p.move_cursor_left();
        p.push_char('X');
        assert_eq!((p.value.as_str(), p.cursor), ("abXc", 3));
        p.pop_char();
        assert_eq!((p.value.as_str(), p.cursor), ("abc", 2));
        p.delete_char();
        assert_eq!(p.value.as_str(), "ab");
    }

    fn trash_confirm() -> InputPrompt {
        InputPrompt::confirm(
            InputPurpose::TreeDelete { paths: Vec::new() },
            "Move 'scratch.py' to Trash?",
            "Enter or Y to trash · Esc or N to keep",
        )
    }

    /// #855: a confirm takes nothing typed or pasted, so `no` cannot sit in
    /// a field that Enter then ignores; Enter answers with no value.
    #[test]
    fn a_confirm_has_no_field_to_type_into() {
        let mut p = trash_confirm();
        for c in "no".chars() {
            p.push_char(c);
        }
        assert_eq!(p.value, "");
        assert_eq!(p.submit_value().as_deref(), Some(""));
    }

    /// #855: the box shows which keys answer it, where the prefilled value
    /// hid that hint, and draws no caret.
    #[test]
    fn a_confirm_draws_its_keys_and_no_caret() {
        let mut p = trash_confirm();
        let screen = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(screen);
        render_input_prompt(&mut p, screen, &mut buf, crate::theme::Theme::BLACK);
        let r = p.last_rect;
        assert_eq!(r.height, 3, "the title and one line of keys");
        let row: String = (r.x + 2..r.x + r.width - 2)
            .map(|x| buf[(x, r.y + 1)].symbol().to_string())
            .collect();
        assert!(
            row.starts_with("Enter or Y to trash · Esc or N to keep"),
            "{row}"
        );
        let caret = (r.y..r.y + r.height).any(|y| {
            (r.x..r.x + r.width).any(|x| buf[(x, y)].modifier.contains(Modifier::SLOW_BLINK))
        });
        assert!(!caret, "a confirm has no caret");
    }
}
