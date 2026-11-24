//! Saving (save all, auto save, format on save, sudo save, encoding loss), unsaved
//! prompts, hot exit, session save and restore, Revert, New Untitled, Save As and the
//! launch file.

use super::*;
use crate::widgets::confirm_frame::{ConfirmFrame, Inset, confirm_width, draw_confirm};

/// What an unsaved-changes prompt (#862) goes on to do once it is answered.
/// Everything that would drop a dirty buffer asks first: a quit that goes
/// ahead removes the hot-exit backup, so the prompt is the last chance to
/// save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UnsavedExit {
    /// Quit croft (Ctrl+Q, vim `:qa`).
    Quit,
    /// Hand a remote croft back to the local one (Cmd+Shift+L), which
    /// ends this process as a quit does.
    DropToLocal,
    /// Leave this croft for a workspace on `host`: the local session ends
    /// and a later return starts a fresh one.
    RemoteLaunch { host: String, path: Option<String> },
    /// The croft on `host` is ready and this one is about to hand it the
    /// terminal, ending here. Asked about edits made since the connect was
    /// asked for, which can be minutes earlier (an install ran between).
    RemoteHandoff { host: String },
    /// Close the focused group's tab at `idx`. `path` is the file it showed
    /// when asked, so an answer never lands on a tab that moved into its
    /// place meanwhile.
    CloseTab { idx: usize, path: Option<PathBuf> },
}

impl UnsavedExit {
    /// The prompt's name for going on: "Save all and quit", "Did not quit".
    pub(super) fn verb(&self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::DropToLocal => "return to local",
            Self::RemoteLaunch { .. } | Self::RemoteHandoff { .. } => "connect",
            Self::CloseTab { .. } => "close",
        }
    }
}

/// Whether closing `ed` would lose text only it holds (#862): unsaved
/// changes to a file, or an untitled buffer that was typed into and still
/// holds something. Never a scratch tab's edits (#951): croft made its text
/// and no save can write it, so the close prompt would have nothing to offer.
pub(super) fn holds_unsaved(ed: &Editor) -> bool {
    ed.dirty && !ed.scratch && (ed.path.is_some() || ed.lines.iter().any(|l| !l.is_empty()))
}

impl LossySave {
    /// The file's name, as the prompt calls it.
    pub(super) fn name(&self) -> String {
        self.path.as_deref().and_then(Path::file_name).map_or_else(
            || String::from("This file"),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// The prompt's lines: what is wrong, what saving does about it, and
    /// how to keep what would be lost.
    pub(super) fn text(&self) -> [String; 3] {
        let name = self.name();
        let enc = self.encoding;
        if self.chars.is_empty() {
            return [
                format!("{name} has bytes that are not valid {enc}, shown as \u{fffd}."),
                String::from(
                    "Saving writes \u{fffd} in their place: the original bytes are lost for good.",
                ),
                String::from(
                    "To keep them, undo your edits (Cmd+Z), then Reopen with Encoding from the status bar.",
                ),
            ];
        }
        let shown: Vec<String> = self.chars.iter().map(char::to_string).collect();
        [
            format!("{enc} cannot represent {} in {name}.", shown.join(" ")),
            String::from(
                "Saving writes them as &#\u{2026}; references: the characters are lost for good.",
            ),
            String::from("To keep them, take them out, or undo the edits that added them (Cmd+Z)."),
        ]
    }

    /// [`Self::text`] short enough to be read out whole with the keys, under
    /// `a11y::MAX_LINE`, the recovery hint included.
    pub(super) fn spoken(&self) -> [String; 3] {
        let name = self.name();
        let enc = self.encoding;
        if self.chars.is_empty() {
            return [
                format!("{name} has invalid {enc} bytes"),
                String::from("Saving turns them into \u{fffd} for good"),
                String::from("To keep them, undo, then Reopen with Encoding"),
            ];
        }
        let shown: Vec<String> = self.chars.iter().map(char::to_string).collect();
        [
            format!("{enc} cannot represent {} in {name}", shown.join(" ")),
            String::from("Saving writes &#\u{2026}; references for good"),
            String::from("To keep them, remove them or undo"),
        ]
    }
}

impl App {
    /// Count of open editors with unsaved edits, across the active group and any
    /// split groups, deduped by path (a file open in two splits is one unsaved
    /// file; each dirty path-less buffer counts once). Drives the Explorer
    /// activity badge, mirroring VS Code, and Save All's "Saved N editors"
    /// (#852).
    pub(super) fn unsaved_count(&self) -> usize {
        let mut paths: std::collections::HashSet<&std::path::Path> =
            std::collections::HashSet::new();
        let mut pathless = 0usize;
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        // A scratch tab's edits are nothing a save writes (#951).
        for ed in groups
            .flat_map(|g| g.editors.iter())
            .filter(|e| e.dirty && !e.scratch)
        {
            match ed.path.as_deref() {
                Some(p) => {
                    paths.insert(p);
                }
                None => pathless += 1,
            }
        }
        paths.len() + pathless
    }

    /// The unsaved-changes prompt's words: its headline, the files, and
    /// what S and D do after their key letter ("ave and close"). Shared by
    /// the modal and what screen reader mode reads out (#1584).
    pub(super) fn unsaved_confirm_text(
        &self,
        exit: &UnsavedExit,
    ) -> (String, String, String, String) {
        match exit {
            UnsavedExit::CloseTab { idx, .. } => (
                crate::i18n::tr("This tab has unsaved changes:").into_owned(),
                self.editor.tab_display_label(*idx),
                String::from("ave and close"),
                String::from("on't save"),
            ),
            other => {
                let files = self.unsaved_files();
                let verb = other.verb();
                (
                    if files.len() == 1 {
                        String::from("1 file has unsaved changes:")
                    } else {
                        format!("{} files have unsaved changes:", files.len())
                    },
                    files.join(", "),
                    format!("ave all and {verb}"),
                    format!("iscard and {verb}"),
                )
            }
        }
    }

    /// The unsaved-changes prompt (#862): what would be lost and the three
    /// ways out. Amber like Replace All's rather than red, since its default
    /// (S / Enter) keeps the edits and only D throws anything away.
    pub(super) fn render_unsaved_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(exit) = self.pending_unsaved.as_ref() else {
            return;
        };
        let (headline, names, save, discard) = self.unsaved_confirm_text(exit);
        let title = format!(" {} ", crate::i18n::tr("UNSAVED CHANGES"));
        let frame_spec = ConfirmFrame {
            title: &title,
            accent: self.theme.ui(Color::Rgb(0xe7, 0xa7, 0x3c)),
            title_fg: Color::Black,
            height: 8,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let key = |k: &'static str, color: Color| {
                Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
            };
            let body = ratatui::text::Text::from(vec![
                Line::from(Span::styled(
                    headline,
                    Style::default().fg(self.theme.ui(Color::White)),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    truncate_for_display(&names, inner.width as usize),
                    Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
                )),
                Line::from(""),
                Line::from(vec![
                    key("[S]", Color::Green),
                    Span::raw(format!("{save}   ")),
                    key("[D]", Color::Red),
                    Span::raw(format!("{discard}   ")),
                    key("[Esc]", self.theme.ui(Color::White)),
                    Span::raw(" cancel"),
                ]),
            ]);
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
    }

    /// The lossy-save prompt (#1053): what the file's encoding would destroy,
    /// and the two answers. Red, since its one action is irreversible, and
    /// tall enough for every wrapped line, so no part of the warning is ever
    /// cut.
    pub(super) fn render_lossy_save_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(asked) = self.pending_lossy_save.as_ref() else {
            return;
        };
        let [what, consequence, keep] = asked.text();
        let inner_w = confirm_width(frame.area()).saturating_sub(4);
        let rows = |s: &str| wrap_cells_variable_width(s, inner_w, inner_w).len() as u16;
        let danger = self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70));
        let frame_spec = ConfirmFrame {
            title: " SAVE WOULD LOSE DATA ",
            accent: danger,
            title_fg: Color::Black,
            // Three texts, a blank line after each, and the keys.
            height: 2 + rows(&what) + rows(&consequence) + rows(&keep) + 4,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let key = |k: &'static str, color: Color| {
                Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
            };
            let text = self.theme.ui(Color::White);
            let dim = self.theme.ui(Color::Rgb(0x9a, 0xa4, 0xb8));
            let body = ratatui::text::Text::from(vec![
                Line::from(Span::styled(what, Style::default().fg(text))),
                Line::from(""),
                Line::from(Span::styled(
                    consequence,
                    Style::default().fg(danger).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(keep, Style::default().fg(dim))),
                Line::from(""),
                Line::from(vec![
                    key("[S]", Color::Red),
                    Span::raw(" save anyway   "),
                    key("[Esc]", text),
                    Span::raw(" keep the file as it is"),
                ]),
            ]);
            frame.render_widget(
                Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: false }),
                inner,
            );
        });
    }

    /// Open `path` as a pinned editor tab and focus the editor. Used at launch
    /// for `croft <file>` and `croft <root> --open-file <path>` (the new
    /// window spawned by Move / Copy into New Window opens the handed-off file
    /// this way). Best-effort: an unreadable file, or a missing one whose
    /// folder is missing too, leaves the editor on the welcome screen.
    /// A relative `path` is taken under the workspace root, so the tab gets
    /// the absolute path its language server is keyed by; a file that can't
    /// be opened says why instead of opening nothing (#1193). Returns whether
    /// it opened.
    pub fn open_file_at_launch(&mut self, path: &Path) -> bool {
        let path = self.workspace_root().join(path);
        // `croft new.md` for a file not written yet opens an empty buffer
        // that the first save creates (#1157).
        let new = std::fs::symlink_metadata(&path).is_err();
        match self.editor.open_new_pinned(&path) {
            Ok(_) => {
                if new {
                    self.status =
                        format!("New file {}: saving creates it", self.status_path(&path));
                }
                // Launch is not a user focus gesture: collapsing here would
                // hide the sidebar before the user has interacted at all
                // (#260).
                self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
                self.sync_open_file_poll_mtime();
                true
            }
            Err(e) => {
                self.status = format!("Couldn't open {}: {e}", path.display());
                false
            }
        }
    }

    /// The session an update relaunch hands to the new process: every
    /// unsaved buffer, a merge editor's Result included, since the
    /// relaunch exits at once and the Result exists nowhere else.
    pub fn capture_session_state(&self) -> crate::session_state::SessionState {
        self.capture_session_state_with(true)
    }

    /// `keep_merge_results` false leaves a merge editor's unsaved Result
    /// out (#1588), for hot exit, whose backup outlives the conflict it was
    /// taken from.
    pub(super) fn capture_session_state_with(
        &self,
        keep_merge_results: bool,
    ) -> crate::session_state::SessionState {
        let mut tabs = Vec::new();
        let mut active_tab = 0;
        let mut active_path: Option<PathBuf> = None;
        for (idx, ed) in self.editor.editors.iter().enumerate() {
            // Only plain text file tabs survive a re-exec; diff / sheet /
            // image tabs are derived views with no path to reopen from. A
            // grid with unsaved edits is the exception (#1593): its edits
            // are kept, and the file reopens in the grid under them.
            if ed.has_non_text_view() {
                if let Some(tab) = sheet_tab_state(ed) {
                    if idx == self.editor.active_index() {
                        active_tab = tabs.len();
                    }
                    tabs.push(tab);
                }
                continue;
            }
            // An untitled buffer has no file to reopen, so it is carried only
            // for its unsaved text; a blank one would reappear as clutter. A
            // scratch tab has no file either (#951): carried, it came back as
            // a tab on its label, whose save wrote a file of that name.
            if (ed.path.is_none() && !ed.dirty) || ed.scratch {
                continue;
            }
            let path = ed.path.clone();
            // A symbol tab is a view over its file's tab (#369): the file tab
            // carries the text, so only an orphaned symbol tab is kept, as a
            // plain file tab, to keep its unsaved edits.
            if let Some(path) = path.clone().filter(|_| ed.symbol_view.is_some())
                && (self.editor.find_tab_with_path(&path).is_some()
                    || tabs.iter().any(|t: &crate::session_state::OpenTabState| {
                        t.path.as_ref() == Some(&path)
                    }))
            {
                if idx == self.editor.active_index() {
                    active_path = Some(path);
                }
                continue;
            }
            if idx == self.editor.active_index() {
                active_tab = tabs.len();
            }
            // A merge editor's buffer is its Result pane, not an edit of the
            // file (#1588): kept in a hot-exit backup it came back as a plain
            // tab whose Ctrl+S wrote the Result over the user's own
            // resolution, so there the tab reopens on the file as it is on
            // disk. A relaunch carries it, or the Result would be lost.
            let dirty = ed.dirty && (keep_merge_results || ed.merge.is_none());
            let unsaved_text = if dirty {
                Some(ed.lines.join("\n"))
            } else {
                None
            };
            tabs.push(crate::session_state::OpenTabState {
                path,
                cursor_row: ed.cursor_row,
                cursor_col: ed.cursor_col,
                scroll: ed.scroll,
                scroll_col: ed.scroll_col,
                dirty,
                unsaved_text,
                sheet_cells: Vec::new(),
            });
        }
        // The other editor groups too: their tabs are not in `self.editor`,
        // and a relaunch dropped the unsaved text of every split but the
        // focused one. Only dirty tabs are carried; a file already captured
        // (the same file open in two splits) keeps the dirty copy.
        for group in self.editor_layout.inactive_groups() {
            for ed in &group.editors {
                if let Some(tab) = sheet_tab_state(ed) {
                    // A clean tab of the same file (the focused split's,
                    // or one reopened as text) gives way to the grid's
                    // unsaved edits rather than drop them.
                    match tabs.iter_mut().find(|t| t.path == tab.path) {
                        Some(existing) if !existing.dirty => *existing = tab,
                        Some(_) => {}
                        None => tabs.push(tab),
                    }
                    continue;
                }
                if ed.has_non_text_view()
                    || !ed.dirty
                    || ed.scratch
                    || (!keep_merge_results && ed.merge.is_some())
                {
                    continue;
                }
                let unsaved = Some(ed.lines.join("\n"));
                if let Some(existing) = ed
                    .path
                    .as_ref()
                    .and_then(|p| tabs.iter_mut().find(|t| t.path.as_ref() == Some(p)))
                {
                    if !existing.dirty {
                        existing.dirty = true;
                        existing.unsaved_text = unsaved;
                    }
                    continue;
                }
                tabs.push(crate::session_state::OpenTabState {
                    path: ed.path.clone(),
                    cursor_row: ed.cursor_row,
                    cursor_col: ed.cursor_col,
                    scroll: ed.scroll,
                    scroll_col: ed.scroll_col,
                    dirty: true,
                    unsaved_text: unsaved,
                    sheet_cells: Vec::new(),
                });
            }
        }
        if let Some(path) = active_path
            && let Some(i) = tabs.iter().position(|t| t.path.as_ref() == Some(&path))
        {
            active_tab = i;
        }
        crate::session_state::SessionState {
            workspace_root: self.workspace_root().to_path_buf(),
            tabs,
            active_tab,
            sidebar_view: sidebar_view_label(self.sidebar_view).to_string(),
            sidebar_width: self.sidebar_width,
            terminal_height: self.terminal_height,
            focus_editor: self.focus == Pane::Editor,
            // A path that is not UTF-8 cannot be a JSON key, and one such
            // mark would fail the whole save.
            bookmarks: self
                .all_bookmarks()
                .into_iter()
                .filter(|(path, _)| path.to_str().is_some())
                .collect(),
            // Filled by the clean-exit save, the one a later launch may
            // find the files changed since.
            bookmark_text: std::collections::HashMap::new(),
        }
    }

    /// Returns the xlsx cells (#1593) it could not put back, as the tabs
    /// they belong to: their workbook did not open in the grid, or lacks
    /// their worksheet or row. The caller keeps them for a later launch.
    pub(super) fn apply_session_state(
        &mut self,
        state: &crate::session_state::SessionState,
    ) -> Vec<crate::session_state::OpenTabState> {
        let mut left = Vec::new();
        for tab in &state.tabs {
            let has_cells = tab.dirty && !tab.sheet_cells.is_empty();
            let unapplied = match self.restore_open_tab(tab) {
                Some((_, unapplied)) => unapplied,
                None if has_cells => tab.sheet_cells.clone(),
                None => Vec::new(),
            };
            if has_cells && !unapplied.is_empty() {
                let mut kept = tab.clone();
                kept.sheet_cells = unapplied;
                left.push(kept);
            }
        }
        // Every tab is its own editor with its own marks: hand each the saved
        // set once its file is open, trimmed to the lines the file still has.
        self.restore_bookmarks_everywhere(&state.bookmarks);
        if state.active_tab < self.editor.tab_count() {
            self.editor.select(state.active_tab);
        }
        if let Some(view) = sidebar_view_from_label(&state.sidebar_view)
            // A CodeQL view saved before the extension was disabled restores
            // to the Explorer rather than to a side bar with no icon.
            && (view != SidebarView::CodeQL || self.is_extension_enabled("codeql"))
        {
            self.set_sidebar_view(view);
        }
        self.sidebar_width = state.sidebar_width;
        self.terminal_height = state.terminal_height;
        self.focus = if state.focus_editor {
            Pane::Editor
        } else {
            Pane::Tree
        };
        // Every pane's focused look follows `focus` only through here: without
        // it a restored editor focus took keys while the editor drew as
        // unfocused and the Explorer kept its focused look (#2068).
        self.sync_focus_flags();
        left
    }

    /// This workspace's session file, saved at each clean exit.
    pub(super) fn saved_session_path(&self) -> Option<PathBuf> {
        let dir = self.session_dir.as_deref()?;
        Some(crate::session_state::saved_path(dir, self.workspace_root()))
    }

    /// Save the session for the next launch of this workspace (a clean
    /// exit): its tabs, layout and bookmarks, without unsaved text.
    pub(super) fn save_session_for_next_launch(&mut self) -> Result<()> {
        let Some(file) = self.saved_session_path() else {
            return Ok(());
        };
        // A tab closed since the last tick still holds the only copy of
        // its file's marks.
        self.settle_bookmarks();
        let mut state = self
            .capture_session_state_with(false)
            .without_unsaved_text();
        // A clean exit throws unsaved edits away (the prompt asked first),
        // and the next launch opens the file as it is on disk: a mark that
        // moved with those edits goes back to its line there. Each mark on
        // an open file also keeps what its line reads there, for a launch
        // that finds the file changed since (see
        // `SessionState::relocate_bookmarks_on_disk`).
        for (path, set) in state.bookmarks.iter_mut() {
            let Some(source) = self
                .bookmark_source(path)
                .filter(|s| s.path.as_deref() == Some(path.as_path()))
            else {
                continue;
            };
            let disk;
            let lines: &[String] = if source.dirty {
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                // Decoded as the tab decoded it, or a UTF-16 file matches no
                // line of its buffer.
                let (text, _, _) = source.encoding.decode(&bytes);
                disk = crate::widgets::editor::split_into_lines(&text);
                *set = crate::widgets::editor::carry_marks(set, &source.lines, &disk);
                &disk
            } else {
                &source.lines
            };
            let text = set
                .iter()
                .filter_map(|&line| Some((line, lines.get(line.checked_sub(1)?)?.clone())))
                .collect();
            state.bookmark_text.insert(path.clone(), text);
        }
        state.bookmarks.retain(|_, set| !set.is_empty());
        state
            .bookmark_text
            .retain(|path, _| state.bookmarks.contains_key(path));
        state.save(&file)
    }

    /// Read the session the last clean exit saved on a thread of its own,
    /// once the first frame is up, for [`Self::poll_saved_session`] to
    /// apply. `tabs` false brings back only its bookmarks (hot exit
    /// restored the tabs). A file saved for another workspace is ignored,
    /// and so is a missing one: the first launch has none. The file stays,
    /// so a crash before the next clean exit still has it.
    pub(super) fn read_saved_session(&mut self, tabs: bool) {
        let Some(path) = self.saved_session_path() else {
            return;
        };
        let root = self.workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        crate::startup_gate::spawn_after_first_frame(move || {
            if let Ok(mut state) = crate::session_state::SessionState::load(&path)
                && state.is_of(&root)
            {
                state.relocate_bookmarks_on_disk();
                let _ = tx.send(state);
            }
        });
        self.saved_session = Some((rx, tabs));
    }

    /// Apply the saved session once it is read (see
    /// [`Self::read_saved_session`]). Its tabs come back only into an
    /// editor still showing its blank first tab: a file the launch opened,
    /// or one the user opened first, is not covered over. Returns whether
    /// anything changed, for a redraw.
    pub(super) fn poll_saved_session(&mut self) -> bool {
        let Some((rx, tabs)) = self.saved_session.as_ref() else {
            return false;
        };
        let state = match rx.try_recv() {
            Ok(state) => state,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.saved_session = None;
                return false;
            }
        };
        let tabs = *tabs;
        self.saved_session = None;
        if tabs && self.editor.is_blank_initial() && !state.tabs.is_empty() {
            // No unsaved text, so nothing is left over to keep.
            let _ = self.apply_session_state(&state);
            self.sync_open_file_poll_mtime();
        } else {
            self.restore_bookmarks_everywhere(&state.bookmarks);
            self.carry_saved_marks_onto_unsaved_tabs(&state.bookmarks);
        }
        true
    }

    /// The saved marks sit on each file's lines on disk (a clean exit saves
    /// them there), but a tab hot exit brought back holds unsaved edits:
    /// carry its file's marks from the file on disk onto its own lines.
    pub(super) fn carry_saved_marks_onto_unsaved_tabs(
        &mut self,
        marks: &std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    ) {
        let in_splits = self
            .editor_layout
            .inactive_groups_mut()
            .flat_map(|g| g.editors.iter_mut());
        for ed in self.editor.editors.iter_mut().chain(in_splits) {
            let Some(path) = ed.path.clone().filter(|_| ed.dirty) else {
                continue;
            };
            let Some(set) = marks.get(&path) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let (text, _, _) = ed.encoding.decode(&bytes);
            let disk = crate::widgets::editor::split_into_lines(&text);
            let carried = crate::widgets::editor::carry_marks(set, &disk, &ed.lines);
            ed.adopt_bookmarks(&path, Some(&carried));
        }
    }

    /// Keep the xlsx cells an update relaunch could not put back (#1593)
    /// in a hot-exit backup of their own, which the next launch restores
    /// from as it would a killed croft's. Returns whether they are on disk.
    pub(super) fn hold_unrestored_tabs(
        &mut self,
        tabs: Vec<crate::session_state::OpenTabState>,
    ) -> bool {
        if tabs.is_empty() {
            return true;
        }
        let Some(own) = self.hot_exit_path() else {
            return false;
        };
        let Some(path) = crate::hot_exit::free_path_beside(&own) else {
            return false;
        };
        let backup = crate::hot_exit::Backup {
            workspace_root: self.workspace_root().to_path_buf(),
            buffers: tabs
                .into_iter()
                .map(|tab| crate::hot_exit::Buffer {
                    tab,
                    disk_stamp: None,
                })
                .collect(),
        };
        backup.save(&path).is_ok()
    }

    /// Reopen one captured tab, the update relaunch's or hot exit's (#862):
    /// its file at the saved cursor and scroll, with its unsaved text over
    /// what the file holds now. Returns the tab, with the xlsx cells of
    /// `tab` it could not put back (#1593), or `None` when there was nothing
    /// to bring back.
    pub(super) fn restore_open_tab(
        &mut self,
        tab: &crate::session_state::OpenTabState,
    ) -> Option<(&mut Editor, Vec<crate::sheet::SheetCellEdit>)> {
        let unsaved = tab.unsaved_text.as_deref().filter(|_| tab.dirty);
        let opened = tab
            .path
            .as_deref()
            .is_some_and(|p| self.editor.open_pinned(p).is_ok());
        let ed = if opened {
            let path = tab.path.as_deref();
            self.editor
                .editors
                .iter_mut()
                .find(|e| e.path.as_deref() == path)?
        } else if unsaved.is_some() {
            // An untitled buffer, or a file that can no longer be read
            // (deleted, permissions changed while the update ran): the
            // unsaved text is the only copy, so it comes back (below) as a
            // dirty tab rather than being dropped.
            self.editor.open_unreadable_tab(tab.path.clone())
        } else {
            return None;
        };
        if ed.sheet.is_some() {
            // A grid's edits go back into the grid (#1593); the text side
            // of a sheet tab is never its content.
            let unapplied = ed.restore_sheet_edits(unsaved, &tab.sheet_cells);
            return Some((ed, unapplied));
        }
        if tab.dirty && !tab.sheet_cells.is_empty() {
            // An xlsx that opened as text or hex (no longer readable as a
            // workbook): none of its cells went back.
            return Some((ed, tab.sheet_cells.clone()));
        }
        if let Some(text) = unsaved {
            // As an edit, not a bare assignment: the text is re-highlighted
            // (it kept the disk file's colours) and the LSP and the rest
            // resync to it. It also stamps the edit time auto save keys on.
            ed.restore_unsaved_text(text);
        }
        let max_row = ed.lines.len().saturating_sub(1);
        ed.cursor_row = tab.cursor_row.min(max_row);
        let max_col = ed.lines.get(ed.cursor_row).map_or(0, |l| l.chars().count());
        ed.cursor_col = tab.cursor_col.min(max_col);
        ed.scroll = tab.scroll;
        ed.scroll_col = tab.scroll_col;
        Some((ed, Vec::new()))
    }

    /// Whether formatting the current buffer is supported by an active server.
    /// Gates format-on-save so a file whose server advertises no formatter is
    /// saved directly rather than waiting on a reply that never mutates it.
    pub(super) fn editor_language_supports_formatting(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_formatting)
    }

    /// Whether `Cmd+S` should format before writing: the pref is on, a
    /// formatter is available, and the active tab is a real editable code file
    /// (not a diff/sheet/image preview). `supports_formatting` is passed in so
    /// the decision is pure and unit-testable.
    pub(super) fn format_on_save_eligible(&self, supports_formatting: bool) -> bool {
        self.format_on_save_for(editor_scope_id(&self.editor))
            && supports_formatting
            && self.editor.path.is_some()
            && self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
    }

    /// The formatter the settings name for the active file when croft does
    /// not run it (#1499), e.g. `biomejs.biome` for a TypeScript file.
    /// `prettier` says croft formats the file with the project's own
    /// Prettier (#1255), so the Prettier extension named for it is not
    /// foreign.
    pub(super) fn foreign_formatter(&self, prettier: bool) -> Option<String> {
        let lang = self
            .editor
            .path
            .as_deref()
            .and_then(crate::lsp::Language::from_path)
            .map(|l| l.lsp_id());
        crate::default_formatter::foreign(
            self.default_formatter.as_deref(),
            &self.language_formatters,
            lang,
        )
        .filter(|f| !(prettier && f.eq_ignore_ascii_case("esbenp.prettier-vscode")))
        .map(str::to_owned)
    }

    /// Save the active file as typed because the project formats it with
    /// `formatter`, which croft does not run (#1499): formatting it with
    /// the language server instead rewrites lines the project's formatter
    /// laid out. The first such save per formatter says why.
    pub(super) fn save_for_foreign_formatter(&mut self, formatter: String) {
        self.write_current_to_disk();
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        if self.path_is_saved(&path) && self.foreign_formatters_noted.insert(formatter.clone()) {
            self.status = format!(
                "Saved {} without formatting: this project formats with {formatter}, which croft does not run",
                self.status_path(&path)
            );
        }
    }

    /// Arm the deferred write for the active tab, ahead of the format request
    /// whose reply will complete it.
    ///
    /// The latch holds one path because the manager tracks one
    /// `format_request_id`: arming a second file abandons the first reply, so
    /// simply overwriting the latch dropped the first file's save entirely,
    /// leaving it dirty and unwritten with nothing reported. Flush it here
    /// instead. It goes to disk unformatted, which is the honest outcome once
    /// its formatter reply can no longer arrive.
    pub(super) fn arm_deferred_save(&mut self) {
        if let Some(pending) = std::mem::take(&mut self.save_after_format)
            && self.editor.path.as_deref() != Some(pending.as_path())
        {
            self.write_tab_to_disk(&pending);
        }
        self.save_after_format = self.editor.path.clone();
        self.format_save_deadline = Some(std::time::Instant::now() + FORMAT_ON_SAVE_TIMEOUT);
    }

    /// Write the deferred buffer to disk once its format reply has landed.
    /// A no-op unless [`Self::save`] armed `save_after_format`.
    pub(super) fn complete_pending_save(&mut self) {
        self.format_save_deadline = None;
        let Some(path) = std::mem::take(&mut self.save_after_format) else {
            return;
        };
        // Write the tab the save was ASKED for. The reply can land long after
        // the user moved on, and `write_current_to_disk` targets whatever is
        // active — which wrote an unrequested file and left this one dirty.
        if self.editor.path.as_deref() == Some(path.as_path()) {
            self.write_current_to_disk();
        } else {
            self.write_tab_to_disk(&path);
        }
        // As in `save`: the file's other tabs hold the text just written.
        self.sync_symbol_views();
        if let Some((op, root)) = self.scm_after_format_save.take() {
            self.finish_scm_after_save(op, &root);
        }
    }

    /// The auto-save delay after the last edit, VS Code's
    /// `files.autoSaveDelay` default.
    pub(super) const AUTO_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(1000);

    /// Write every dirty, conflict-free buffer whose last edit is older
    /// than the delay — the whole `files.autoSave: afterDelay` feature.
    /// Runs every frame; returns true when anything was saved so the
    /// caller redraws (dirty markers, badges, status).
    pub fn tick_auto_save(&mut self) -> bool {
        // `onFocusChange` fires on the transition, so the signature is
        // sampled every tick regardless of which modes are enabled.
        let focus_changed = self.note_focus_signature();
        let mut changed = false;
        if self.auto_save_on_focus_change && focus_changed {
            changed = self.sweep_auto_save(false);
        }
        if self.auto_save {
            changed |= self.sweep_auto_save(true);
        }
        changed
    }

    /// The terminal window croft runs in lost focus (#1622): alt-tab to the
    /// browser, another tmux window. With auto save "onWindowChange" (or
    /// `onFocusChange`) every dirty buffer is written now, the one being
    /// edited included, since the whole window is what lost focus; VS Code
    /// does the same. A `Cmd+K` leader waiting for its second key stops
    /// waiting, as VS Code's does when its window loses focus. Returns true
    /// when anything changed on screen.
    pub fn on_window_focus_lost(&mut self) -> bool {
        let left_chord = self.cmd_k_leader.take().is_some();
        if !self.auto_save_on_focus_change {
            return left_chord;
        }
        // An auto save: the caret lines keep their trailing whitespace,
        // since the user may be mid-word when they alt-tab away (#1455).
        self.sweep_dirty_buffers(false, false, true) || left_chord
    }

    /// Sample the current focus (pane, active tab, active path) and report
    /// whether it moved since the last sample. The first sample never
    /// counts as a change, so launching croft writes nothing on its own.
    pub(super) fn note_focus_signature(&mut self) -> bool {
        let pane = match self.focus {
            Pane::Tree => 0usize,
            Pane::Editor => 1,
            Pane::Terminal => 2,
        };
        let sig = (pane, self.editor.active_index(), self.editor.path.clone());
        match self.last_focus_signature.replace(sig.clone()) {
            None => false,
            Some(prev) => prev != sig,
        }
    }

    /// The shared auto-save sweep. `require_delay` selects the mode:
    /// `afterDelay` waits out [`Self::AUTO_SAVE_DELAY`] since the last
    /// edit and writes every dirty buffer; `onFocusChange` writes
    /// immediately but skips the editor that currently HAS focus (it has
    /// not lost it yet). Returns true when anything changed on screen.
    pub(super) fn sweep_auto_save(&mut self, require_delay: bool) -> bool {
        let skip_active = !require_delay && self.focus == Pane::Editor;
        self.sweep_dirty_buffers(require_delay, skip_active, true)
    }

    /// Write every dirty buffer the auto-save rules allow, in every split,
    /// leaving the active tab alone when `skip_active`. Shared by the
    /// auto-save sweep and File: Save All (#852). `automatic` is the
    /// auto-save sweep, whose writes spare the caret lines' trailing
    /// whitespace; Save All's are explicit saves (#1455).
    pub(super) fn sweep_dirty_buffers(
        &mut self,
        require_delay: bool,
        skip_active: bool,
        automatic: bool,
    ) -> bool {
        // Each saved tab's own map rides along (#349): the recorder must not
        // look it up by path afterwards, since a split can hold the same file
        // in a second buffer with a different map.
        let mut saved_paths: Vec<(PathBuf, crate::provenance::Provenance, Option<Vec<u8>>)> =
            Vec::new();
        // And each saved tab's text, for a server whose didSave carries it
        // (#854).
        let mut saved_texts: Vec<String> = Vec::new();
        // The tabs of a file with a symbol tab mirror each other (#369), so
        // they hold one text: the first write saves them all, and writing a
        // second would read the first as an external change. Settle them
        // before the sweep (which may close a symbol tab, so before any tab
        // index is taken) and mark the rest clean after it. A file live in a
        // collab session is not mirrored, so each of its tabs saves itself:
        // on the owner the second write then meets the first one's stamp and
        // reports a disk conflict, as two split panes of a live file do.
        let settled = self.sync_symbol_views();
        let mirrored: Vec<PathBuf> = self
            .symbol_view_paths()
            .into_iter()
            .filter(|p| !self.symbol_path_is_live(p))
            .collect();
        // The tab that still holds focus is not saved by the focus-change
        // mode: only buffers that LOST focus are written. Save All skips it
        // too, having just saved it the explicit way.
        let keep_focused = skip_active.then(|| self.editor.active_index());
        // Collab guests never write shared (workspace) files; the session
        // owner is the single writer (docs/MULTIPLAYER.md, Phase D).
        let guest = self.is_collab_guest();
        let root = self.tree.root.clone();
        let due = |e: &crate::widgets::editor::Editor| {
            e.dirty
                && e.path.is_some()
                // A scratch tab's path is its label (#951): one stray
                // keystroke wrote a file named after the tab.
                && !e.scratch
                // Preview-kind edits (hex bytes, sheet cells) are
                // EXPLICIT-save only: an automatic write mid-edit could
                // land a half-updated byte sequence, and the serialising
                // kinds normalise quoting the user did not ask to touch.
                // (They also never set last_edit_at, so this guard makes
                // an implicit exclusion structural - #191 review.)
                && !e.has_non_text_view()
                && !e.disk_conflict
                // A file deleted on disk stays deleted until the user saves
                // it (#900): a checkout or `git stash` may have meant it.
                && !(automatic && e.deleted_on_disk())
                // A latched encoding refusal: only an explicit Cmd+S can
                // consent to the lossy write, so retrying here would just
                // re-refuse every tick.
                && !e.encoding_loss
                // A file the user can't write, or protected from writes,
                // is only ever saved on an explicit Cmd+S (#1379).
                && !e.write_protected
                // A merge with conflicts still open: its Result holds only
                // the base text there, and writing it would drop both
                // sides from disk. Complete Merge (or an explicit Cmd+S) is
                // the user's call.
                && !e.merge.as_ref().is_some_and(|m| m.unresolved_count() > 0)
                && !(guest
                    && e.path
                        .as_ref()
                        .is_some_and(|p| collab_file_key(&root, p).is_some()))
                && (!require_delay
                    || e.last_edit_at
                        .is_some_and(|t| t.elapsed() >= Self::AUTO_SAVE_DELAY))
        };
        let mut conflicted: Vec<PathBuf> = Vec::new();
        // "name (encoding)" per refused tab: the refusal message names each
        // tab's OWN encoding, which need not be the active tab's.
        let mut lossy: Vec<String> = Vec::new();
        let mut lossy_paths: Vec<PathBuf> = Vec::new();
        let cleanup = self.save_cleanup;
        let mut sweep = |editors: &mut [crate::widgets::editor::Editor], skip: Option<usize>| {
            for (i, e) in editors
                .iter_mut()
                .enumerate()
                .filter(|(i, e)| Some(*i) != skip && due(e))
            {
                let _ = i;
                if e.path.as_ref().is_some_and(|p| {
                    mirrored.contains(p) && saved_paths.iter().any(|(q, ..)| q == p)
                }) {
                    continue;
                }
                // `save_to_disk` re-checks the disk and flags (never
                // overwrites) an external change; auto save must not
                // arm the force-overwrite path an explicit Cmd+S offers.
                e.save_cleanup = cleanup;
                let saved = if automatic {
                    e.auto_save_to_disk()
                } else {
                    e.save_to_disk()
                };
                match saved {
                    Ok(crate::widgets::editor::SaveOutcome::Saved) => {
                        if let Some(p) = e.path.clone() {
                            // The bytes this editor just wrote, so the
                            // recorder can tell whether they are still the
                            // ones on disk when its worker reads (#349).
                            saved_paths.push((p, e.provenance_to_record(), e.bytes_for_disk()));
                            saved_texts.push(e.document_text());
                        }
                    }
                    // The latch also removes the tab from `due`, so this is
                    // the ONLY chance to tell the user: the FS sweep's
                    // prompt fires on the transition into disk_conflict,
                    // which this save just consumed.
                    // A mirrored sibling holds the same text and stamp, so it
                    // meets the same outcome: each file is reported once.
                    Ok(crate::widgets::editor::SaveOutcome::DiskConflict) => {
                        if let Some(p) = e.path.clone()
                            && !conflicted.contains(&p)
                        {
                            conflicted.push(p);
                        }
                    }
                    // Same transition-only reporting as the conflict arm:
                    // the latch just set removes the tab from `due`, so
                    // this tick is the one chance to say why it stopped
                    // saving. Never arms the lossy write — consent is the
                    // explicit path's alone.
                    // The refusal set `write_protected`, which keeps the
                    // tab out of `due`: the lock on its tab says why.
                    Ok(crate::widgets::editor::SaveOutcome::WriteProtected) => {}
                    Ok(crate::widgets::editor::SaveOutcome::EncodingLoss) => {
                        if let Some(p) = e.path.as_ref()
                            && !lossy_paths.contains(p)
                            && let Some(name) = p.file_name()
                        {
                            lossy_paths.push(p.clone());
                            lossy.push(format!(
                                "{} ({})",
                                name.to_string_lossy(),
                                e.encoding.name()
                            ));
                        }
                    }
                    Err(_) => {}
                }
            }
        };
        sweep(&mut self.editor.editors, keep_focused);
        for group in self.editor_layout.inactive_groups_mut() {
            sweep(&mut group.editors, None);
        }
        if !saved_paths.is_empty() {
            self.sync_symbol_views();
        }
        let had_conflicts = !conflicted.is_empty();
        if had_conflicts {
            self.prompt_disk_conflict(conflicted);
        }
        let had_lossy = !lossy.is_empty();
        if had_lossy {
            // The status line, not the modal prompt: nothing was lost and
            // nothing is racing, so an auto-save refusal must not steal the
            // keyboard mid-typing burst.
            self.status = format!(
                "Auto save paused for {}: the buffer has characters the encoding cannot represent - open the tab and press Cmd+S to resolve",
                lossy.join(", ")
            );
        }
        if saved_paths.is_empty() {
            // A conflict-only (or refusal-only) tick still changed the UI:
            // returning false would defer it to the next incidental redraw.
            return settled || had_conflicts || had_lossy;
        }
        // Mirror the explicit-save path: a config file written by auto save
        // must take effect exactly like one written by Cmd+S — including one
        // saved in a background tab or an inactive split.
        for (path, ..) in &saved_paths {
            self.reload_config_for_path(path);
        }
        if let Some(lsp) = self.lsp.as_ref() {
            for ((path, ..), text) in saved_paths.iter().zip(saved_texts) {
                lsp.save_doc(path.clone(), text);
            }
        }
        for (path, seats, described) in saved_paths {
            self.record_history_snapshot_of(&path, seats, described);
        }
        true
    }

    /// File: Save All (#852, `Cmd+Opt+S` / `Ctrl+Alt+S`): the active tab
    /// saves exactly as `Cmd+S` would (format on save, hex and sheet edits,
    /// the overwrite prompt), then every other dirty buffer in every split
    /// is written by the auto-save rules, which leave a tab that must not
    /// be written blind (a disk conflict, a lossy encoding, an unresolved
    /// merge, a hex or sheet edit) to its own `Cmd+S`.
    pub(super) fn save_all(&mut self) {
        let before = self.unsaved_count();
        if before == 0 {
            self.status = String::from("Save All: nothing to save");
            return;
        }
        // A scratch tab (#951) has nothing to save, nor a refusal to report.
        if self.editor.dirty && !self.editor.scratch {
            self.save();
        }
        let active_status = self.status.clone();
        self.sweep_dirty_buffers(false, true, false);
        // A format-on-save write lands with its formatter reply: it is on
        // its way, not left behind.
        let pending = self
            .save_after_format
            .clone()
            .map(|path| self.status_path(&path));
        let left = self
            .unsaved_count()
            .saturating_sub(usize::from(pending.is_some()));
        let active_refusal = if self.editor.dirty && !self.editor.scratch {
            active_status
        } else {
            String::new()
        };
        self.status = save_all_summary(before, left, pending.as_deref(), active_refusal);
    }

    pub(super) fn toggle_auto_save(&mut self) {
        self.auto_save = !self.auto_save;
        self.status = if self.auto_save {
            String::from("Auto Save: on")
        } else {
            String::from("Auto Save: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_auto_save(self.auto_save));
        }
    }

    /// Flip `files.autoSave: onFocusChange` and persist it, like
    /// [`Self::toggle_auto_save`] does for the delay mode.
    pub(super) fn toggle_auto_save_on_focus_change(&mut self) {
        self.auto_save_on_focus_change = !self.auto_save_on_focus_change;
        self.status = if self.auto_save_on_focus_change {
            String::from("Auto Save on Focus Change: on")
        } else {
            String::from("Auto Save on Focus Change: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_auto_save_on_focus_change(
                self.auto_save_on_focus_change,
            ));
        }
    }

    pub(super) fn save(&mut self) {
        self.save_active_tab();
        // The saved text is every same-file tab's text: settle them now, so
        // none reads as dirty or as changed on disk before the next tick
        // (#369). A format-on-save write lands later and settles them in
        // `complete_pending_save`.
        self.sync_symbol_views();
    }

    /// Save only the active tab; its same-file siblings are left as they
    /// are. Callers go through [`Self::save`].
    pub(super) fn save_active_tab(&mut self) {
        // A hex tab with pending overwrites saves through its own byte
        // path (#173) — never `write_buffer_to_disk`, whose #185 guard
        // refuses every preview kind. Same disk-conflict double-press
        // contract as text saves.
        if self.editor.hex.as_ref().is_some_and(|v| v.has_edits()) {
            use crate::widgets::editor::SaveOutcome;
            let force = std::mem::take(&mut self.force_save_armed);
            match self.editor.hex_save(force) {
                Ok(SaveOutcome::Saved) => {
                    self.status = self.editor.status.clone();
                    // Local history captures the new bytes like every
                    // other successful save path (#191 review).
                    if let Some(path) = self.editor.path.clone() {
                        let seats = self.editor.provenance_to_record();
                        let described = self.editor.bytes_for_disk();
                        self.record_history_snapshot_of(&path, seats, described);
                    }
                }
                Ok(SaveOutcome::DiskConflict) => {
                    self.force_save_armed = true;
                    let name = self
                        .editor
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    self.status = format!(
                        "{name} changed on disk since you opened it - press Cmd+S again to overwrite the changed bytes"
                    );
                }
                Ok(_) => unreachable!("hex_save reports Saved or DiskConflict"),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
            return;
        }
        // A cell still being typed into is part of the save (#1140): commit
        // it the way Enter does, so it reaches the file.
        if let Some(view) = self.editor.sheet.as_mut()
            && view.editable()
            && view.commit_edit().is_some()
        {
            self.editor.mark_dirty();
        }
        // A CSV/TSV sheet with grid edits saves through its own
        // serialisation path (#177), same shape as the hex branch above.
        if self.editor.sheet.as_ref().is_some_and(|v| v.dirty) {
            use crate::widgets::editor::SaveOutcome;
            let force = std::mem::take(&mut self.force_save_armed);
            // Distinct consents (#194 review): overwriting an external
            // disk change and replacing a FORMULA are different
            // decisions, each armed by its own refusal message.
            let overwrite_formulas = std::mem::take(&mut self.formula_overwrite_armed);
            match self.editor.sheet_save(force, overwrite_formulas) {
                Ok(SaveOutcome::Saved) => {
                    self.status = self.editor.status.clone();
                    if let Some(path) = self.editor.path.clone() {
                        let seats = self.editor.provenance_to_record();
                        let described = self.editor.bytes_for_disk();
                        self.record_history_snapshot_of(&path, seats, described);
                    }
                    // Formula cells held back (#178): the NEXT Cmd+S is
                    // the explicit consent, same double-press contract
                    // as disk conflicts.
                    if self.editor.sheet.as_ref().is_some_and(|v| v.dirty) {
                        self.formula_overwrite_armed = true;
                    }
                }
                Ok(SaveOutcome::DiskConflict) => {
                    self.force_save_armed = true;
                    let name = self
                        .editor
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    self.status = format!(
                        "{name} changed on disk since you opened it - press Cmd+S again to overwrite"
                    );
                }
                Ok(_) => unreachable!("sheet_save reports Saved or DiskConflict"),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
            return;
        }
        // A preview tab has nothing to save (and #185: letting the write
        // proceed truncated the previewed file). Friendly no-op here; the
        // choke-point guard in `write_buffer_to_disk` backstops every
        // other path.
        if self.editor.has_non_text_view() {
            // An editable grid or hex view with nothing to write isn't a
            // preview (#1140).
            let editable = self.editor.sheet.as_ref().is_some_and(|v| v.editable())
                || self.editor.hex.is_some();
            self.status = if self
                .editor
                .hex
                .as_ref()
                .is_some_and(|v| v.pending_nibble.is_some())
            {
                String::from("Type the byte's second hex digit, then save")
            } else if editable {
                String::from("No changes to save")
            } else {
                String::from("Nothing to save: this tab is a read-only preview")
            };
            return;
        }
        // An untitled buffer (#1580) has nowhere to go yet: Save is Save As,
        // as in VS Code.
        if self.editor.path.is_none() && self.editor.diff.is_none() {
            self.open_save_as_prompt();
            return;
        }
        // A scratch tab's path is only its label (#951): writing it made a
        // file named `stats.py @ 4affdca` or `Search: mean` in croft's
        // working directory. Pressed from habit, so it says rather than asks.
        if self.editor.scratch {
            self.status = String::from(crate::widgets::editor::SCRATCH_SAVE_REFUSAL);
            return;
        }
        // Guests in a collab session never write shared files: the owner is
        // the single writer, so the FS watcher and history snapshots see one
        // author. The guest's edits already flowed to the owner as ops.
        // Except a file whose bootstrap gave up (no owner answered): the
        // guest is its only author, and refusing meant the work could never
        // persist anywhere. Same when there is NO session at all (relay
        // link down, reconnect pending): with nobody to defer to, blocking
        // strands the offline work in RAM for the whole outage. And when
        // the owner left (#1147): there is nobody to save for this guest.
        if self.owner_saves_active_file() {
            self.status =
                String::from("Shared file: the session owner saves (your edits are already live)");
            return;
        }
        // A save refused for want of permission arms the sudo route for the
        // next Cmd+S of that file (#1379).
        if self
            .sudo_save_armed
            .take()
            .is_some_and(|p| self.editor.path.as_ref() == Some(&p))
        {
            self.save_with_sudo();
            return;
        }
        // A previously-refused save arms a one-shot override: the very next
        // Cmd+S overwrites the external change. Consume it here so it never
        // lingers past the press it was meant for.
        if std::mem::take(&mut self.force_save_armed) {
            use crate::widgets::editor::SaveOutcome;
            self.editor.save_cleanup = self.save_cleanup;
            match self.editor.save_to_disk_force() {
                Ok(SaveOutcome::Saved) => {
                    self.status = self.editor.status.clone();
                    if let Some(path) = self.editor.path.clone() {
                        let seats = self.editor.provenance_to_record();
                        let described = self.editor.bytes_for_disk();
                        self.record_history_snapshot_of(&path, seats, described);
                    }
                    self.lsp_notify_saved();
                    self.reload_config_if_needed();
                }
                // Overwriting the external change was consented to; mangling
                // the buffer's characters was not. Route the refusal through
                // the same prompt-and-rearm as a plain save, keeping the
                // overwrite consent so the NEXT Cmd+S resolves both at once.
                Ok(SaveOutcome::EncodingLoss) => {
                    self.force_save_armed = true;
                    self.prompt_encoding_loss();
                }
                Ok(SaveOutcome::WriteProtected) => {
                    self.force_save_armed = true;
                    self.prompt_write_protected();
                }
                Ok(SaveOutcome::DiskConflict) => {
                    unreachable!("force save never reports a conflict")
                }
                Err(e) => self.report_save_error(e),
            }
            return;
        }
        // Saved again while the formatter has still not answered: write now
        // rather than wait on it a second time (#1491).
        if self.save_after_format.is_some() && self.save_after_format == self.editor.path {
            self.save_without_formatting("saved again before the formatter answered");
            return;
        }
        self.format_and_save(self.editor_language_supports_formatting());
    }

    /// The write at the end of a save: with format on save on and a
    /// formatter for the file, defer it until the format reply arrives
    /// (`drain_lsp_format`, or `drain_prettier_format`, calls
    /// `complete_pending_save`), unless the project names a formatter croft
    /// does not run (#1499).
    pub(super) fn format_and_save(&mut self, supports_formatting: bool) {
        let prettier = self
            .editor
            .path
            .as_deref()
            .and_then(crate::prettier::for_file)
            .is_some();
        if self.format_on_save_eligible(prettier || supports_formatting) {
            if let Some(formatter) = self.foreign_formatter(prettier) {
                self.save_for_foreign_formatter(formatter);
                return;
            }
            self.arm_deferred_save();
            self.start_format_document();
            return;
        }
        self.write_current_to_disk();
    }

    /// Tell the language servers the focused file hit the disk. rust-analyzer
    /// re-runs its check-on-save (`cargo check`, which produces the Rust
    /// PROBLEMS entries) only on `textDocument/didSave`; without this the
    /// panel never refreshes after a save (issue #37).
    pub(super) fn lsp_notify_saved(&mut self) {
        if let (Some(lsp), Some(path)) = (self.lsp.as_ref(), self.editor.path.clone()) {
            lsp.save_doc(path, self.editor.document_text());
        }
    }

    /// Save the open tab for `path` wherever it lives — background tab of the
    /// focused group, or any inactive split — without disturbing the focus.
    /// Used by the deferred format-on-save write when the user has moved on.
    pub(super) fn write_tab_to_disk(&mut self, path: &std::path::Path) {
        use crate::widgets::editor::SaveOutcome;
        let found = self
            .editor
            .editors
            .iter_mut()
            .chain(
                self.editor_layout
                    .inactive_groups_mut()
                    .flat_map(|g| g.editors.iter_mut()),
            )
            .find(|e| e.path.as_deref() == Some(path));
        let Some(editor) = found else {
            // The tab was closed while the formatter was thinking; there is no
            // buffer left to write and the disk file is untouched.
            self.status = String::from("Save skipped: the tab was closed");
            return;
        };
        editor.save_cleanup = self.save_cleanup;
        match editor.save_to_disk() {
            Ok(SaveOutcome::Saved) => {
                let seats = editor.provenance_to_record();
                let described = editor.bytes_for_disk();
                let text = editor.document_text();
                self.status = editor.status.clone();
                self.record_history_snapshot_of(path, seats, described);
                if let Some(lsp) = self.lsp.as_ref() {
                    lsp.save_doc(path.to_path_buf(), text);
                }
                self.reload_config_for_path(path);
            }
            Ok(SaveOutcome::DiskConflict) => {
                self.status = format!(
                    "{} changed on disk since you opened it - open the tab and press Cmd+S twice to overwrite, or close it to keep the disk version",
                    path.display()
                );
            }
            Ok(SaveOutcome::EncodingLoss) => {
                // Both callers route a still-focused tab to
                // `write_current_to_disk`, which arms the press-again consent
                // itself; this function only ever runs for a tab the user
                // moved away from, and such a tab never gets lossy consent —
                // resolving needs it back in front of them.
                self.status = format!(
                    "{} not saved: it has characters {} cannot represent - open the tab and press Cmd+S to resolve",
                    path.display(),
                    editor.encoding.name()
                );
            }
            Ok(SaveOutcome::WriteProtected) => {
                self.status = format!(
                    "{} not saved: it is write-protected - open the tab and press Cmd+S twice to overwrite it",
                    path.display()
                );
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    pub(super) fn write_current_to_disk(&mut self) {
        use crate::widgets::editor::SaveOutcome;
        self.editor.save_cleanup = self.save_cleanup;
        match self.editor.save_to_disk() {
            Ok(SaveOutcome::Saved) => {
                self.status = self.editor.status.clone();
                if let Some(path) = self.editor.path.clone() {
                    let seats = self.editor.provenance_to_record();
                    let described = self.editor.bytes_for_disk();
                    self.record_history_snapshot_of(&path, seats, described);
                }
                self.lsp_notify_saved();
                self.reload_config_if_needed();
            }
            Ok(SaveOutcome::DiskConflict) => {
                self.force_save_armed = true;
                let name = self
                    .editor
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status = format!(
                    "{name} changed on disk since you opened it - press Cmd+S again to overwrite, or close the tab to keep the disk version"
                );
            }
            Ok(SaveOutcome::EncodingLoss) => self.prompt_encoding_loss(),
            Ok(SaveOutcome::WriteProtected) => self.prompt_write_protected(),
            Err(e) => self.report_save_error(e),
        }
    }

    /// A save of the user's own write-protected file was refused (#1379).
    /// Arm the one-shot consent and say what the next Cmd+S will do, the
    /// press-again flow a disk conflict uses.
    pub(super) fn prompt_write_protected(&mut self) {
        self.editor.overwrite_protected_armed = true;
        let name = self
            .editor
            .path
            .as_deref()
            .map(|p| self.status_path(p))
            .unwrap_or_default();
        self.status = format!(
            "{name} is write-protected - press Cmd+S again to overwrite it (its permissions are put back after), or use File: Save As…"
        );
    }

    /// Whether the active tab is a collab-shared file this guest must leave
    /// to the session owner to write (see `save`).
    pub(super) fn owner_saves_active_file(&self) -> bool {
        self.is_collab_guest()
            && self
                .editor
                .path
                .as_ref()
                .and_then(|p| collab_file_key(&self.tree.root, p))
                .is_some_and(|file| {
                    self.collab
                        .as_ref()
                        .is_some_and(|s| !s.is_local_only(&file) && !s.owner_gone())
                })
    }

    /// Report a failed explicit save. One that failed for want of
    /// permission offers the sudo route on the next Cmd+S (#1379): saving
    /// again as the same user could only fail again.
    pub(super) fn report_save_error(&mut self, e: anyhow::Error) {
        if cfg!(unix)
            && crate::widgets::editor::is_permission_denied(&e)
            && let Some(path) = self.editor.path.clone()
        {
            self.status = format!(
                "Save failed: you can't write {} - press Cmd+S again to save it with sudo in a terminal",
                self.status_path(&path)
            );
            self.sudo_save_armed = Some(path);
            return;
        }
        self.status = format!("Save failed: {e}");
    }

    /// File: Save with Sudo (#1379): hand the buffer to `sudo tee` in a new
    /// terminal pane. sudo asks for the password there, so croft never sees
    /// it, and `tee` writes the file in place, so it keeps its owner and
    /// mode. The tab turns clean once the file holds the buffer's bytes.
    pub(super) fn save_with_sudo(&mut self) {
        // A consent to overwrite an external change lasts one press, as in
        // `save`.
        let overwrite = std::mem::take(&mut self.force_save_armed);
        if self.owner_saves_active_file() {
            self.status =
                String::from("Shared file: the session owner saves (your edits are already live)");
            return;
        }
        if self.editor.has_non_text_view() {
            self.status = String::from("Save with Sudo works on text tabs");
            return;
        }
        // A scratch tab's path is its label, not a file (#951).
        let Some(path) = self.editor.path.clone().filter(|_| !self.editor.scratch) else {
            self.status = String::from("Save with Sudo needs a file: use File: Save As… first");
            return;
        };
        if !cfg!(unix) {
            self.status = String::from("Save with Sudo needs sudo, which this system lacks");
            return;
        }
        // The command line takes the path as text, so a name that is not
        // valid UTF-8 could only be passed mangled, writing another file.
        let Some(target) = path.to_str().map(str::to_owned) else {
            self.status = String::from(
                "Save with Sudo can't pass this file's name to the shell: it is not valid UTF-8",
            );
            return;
        };
        // Another process wrote the file since it was read: overwriting
        // that takes the same press-again consent a plain save asks for.
        if !overwrite && (self.editor.disk_conflict || self.editor.disk_changed_externally()) {
            self.editor.disk_conflict = true;
            self.force_save_armed = true;
            self.sudo_save_armed = Some(path.clone());
            self.status = format!(
                "{} changed on disk since you opened it - press Cmd+S again to overwrite it with sudo, or close the tab to keep the disk version",
                self.status_path(&path)
            );
            return;
        }
        // A sudo save of this file still waiting in its terminal could
        // finish after this one and put the older bytes back.
        if self
            .pending_sudo_saves
            .iter()
            .find(|(p, ..)| *p == path)
            .and_then(|(.., pane)| *pane)
            .is_some_and(|pane| self.terminals.iter().any(|t| t.shell_pid() == Some(pane)))
        {
            self.status = format!(
                "A sudo save of {} is still waiting in its terminal: finish it there, or close that terminal first",
                self.status_path(&path)
            );
            return;
        }
        let Some(bytes) = self.editor.prepare_bytes_for_save() else {
            self.status = format!(
                "Not saved: {} can't hold every character in this file - switch encoding via the status bar",
                self.editor.encoding.name()
            );
            return;
        };
        let staged = match stage_sudo_save(&bytes) {
            Ok(staged) => staged,
            Err(e) => {
                self.status = format!("Save with Sudo failed: {e}");
                return;
            }
        };
        let command = sudo_save_command(&target, &staged.to_string_lossy());
        let cwd = self.workspace_root().to_path_buf();
        match PtyTerminal::new(&cwd) {
            Ok(mut term) => {
                term.set_manual_name(Some(String::from("sudo save")));
                let pane = term.shell_pid();
                // Tests stop short of running sudo for real.
                if !cfg!(test) {
                    term.write_input(format!("{command}\r").as_bytes());
                }
                self.insert_terminal(term);
                self.show_terminal = true;
                self.bottom_panel_tab = BottomPanelTab::Terminal;
                self.focus_pane(Pane::Terminal);
                self.pending_sudo_saves.retain(|(p, ..)| *p != path);
                self.pending_sudo_saves.push((path.clone(), bytes, pane));
                self.status = format!(
                    "Saving {} with sudo: enter your password in the terminal",
                    self.status_path(&path)
                );
            }
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                self.status = format!("Could not open a terminal: {e}");
            }
        }
    }

    /// Settle the sudo saves whose file now holds what was handed to `tee`
    /// (#1379): those tabs are saved, not changed on disk. Runs before the
    /// external-change sweep, which would otherwise take the write for
    /// someone else's and raise a conflict over the user's own save. A save
    /// whose tab is gone is dropped; one still in flight waits. Returns
    /// whether a tab was settled.
    pub(super) fn settle_sudo_saves(&mut self) -> bool {
        if self.pending_sudo_saves.is_empty() {
            return false;
        }
        let mut saved = Vec::new();
        for (path, bytes, pane) in std::mem::take(&mut self.pending_sudo_saves) {
            let mut open = false;
            let mut settled = None;
            for e in std::iter::once(&mut self.editor)
                .chain(self.editor_layout.inactive_groups_mut())
                .flat_map(|g| g.editors.iter_mut())
                .filter(|e| e.path.as_ref() == Some(&path))
            {
                open = true;
                if e.settle_save_made_elsewhere(&bytes) && settled.is_none() {
                    settled = Some((
                        e.provenance_to_record(),
                        e.bytes_for_disk(),
                        e.document_text(),
                    ));
                }
            }
            if let Some((seats, described, text)) = settled {
                saved.push((path, seats, described, text));
            } else if open {
                self.pending_sudo_saves.push((path, bytes, pane));
            }
        }
        let Some(last) = saved.last().map(|(path, ..)| path.clone()) else {
            return false;
        };
        // Set first: a reloaded config file reports on itself.
        self.status = format!("Saved {} with sudo", self.status_path(&last));
        // The same follow-ups as every other save path.
        for (path, seats, described, text) in saved {
            self.reload_config_for_path(&path);
            self.approve_saved_edit(&path);
            if let Some(lsp) = self.lsp.as_ref() {
                lsp.save_doc(path.clone(), text);
            }
            self.record_history_snapshot_of(&path, seats, described);
        }
        true
    }

    /// An explicit save was refused because the encoding cannot represent the
    /// buffer (`SaveOutcome::EncodingLoss`). Ask in a modal what to do,
    /// naming exactly what the write would destroy (#1053). Only S there
    /// arms the one-shot lossy consent and saves; the default, Esc, keeps the
    /// file as it is. It used to arm the next Cmd+S and say so in the status
    /// bar, which cut the warning out of the middle of the message at most
    /// widths, so the natural retry destroyed the data unwarned.
    pub(super) fn prompt_encoding_loss(&mut self) {
        let encoding = self.editor.encoding.name();
        self.pending_lossy_save = Some(LossySave {
            path: self.editor.path.clone(),
            encoding,
            chars: self.editor.unmappable_chars(),
        });
        self.status = format!("Not saved: {encoding} cannot hold this file as it is");
    }

    /// S in the lossy-save modal (#1053): consent to the write it named, for
    /// the tab it asked about, and save.
    pub(super) fn confirm_lossy_save(&mut self) {
        let Some(asked) = self.pending_lossy_save.take() else {
            return;
        };
        if self.editor.path != asked.path {
            self.force_save_armed = false;
            self.status = String::from("Not saved: the file asked about is no longer in front");
            return;
        }
        self.editor.lossy_save_armed = true;
        self.save();
    }

    /// Esc in the lossy-save modal (#1053): nothing is written. A disk
    /// conflict overwrite consented to on the way here is dropped too, so the
    /// next Cmd+S starts over rather than half-answered.
    pub(super) fn cancel_lossy_save(&mut self) {
        self.pending_lossy_save = None;
        self.force_save_armed = false;
        self.status = String::from("Not saved: the file on disk is unchanged");
    }

    /// Remote: Drop to Local (`Cmd+K L`, #2071): on a remote session, end it
    /// and hand back to the local croft it was launched from, asking first
    /// about unsaved edits as a quit does. A local croft has nothing to
    /// leave, and says so rather than doing nothing.
    pub(super) fn drop_to_local_if_remote(&mut self) {
        if self.is_remote {
            self.guard_unsaved(UnsavedExit::DropToLocal);
        } else {
            self.status = String::from("Drop to Local: this croft is not a remote session");
        }
    }

    /// Go on with `exit`, unless it would drop unsaved changes (#862): then
    /// raise the prompt that asks whether to save them first. Every quit and
    /// every single-tab close comes through here. The update relaunch alone
    /// does not: its session handoff carries unsaved text across the re-exec.
    pub(super) fn guard_unsaved(&mut self, exit: UnsavedExit) {
        let at_stake = match &exit {
            UnsavedExit::CloseTab { idx, .. } => self.tab_would_lose_edits(*idx),
            _ => !self.unsaved_files().is_empty(),
        };
        if at_stake {
            self.pending_unsaved = Some(exit);
        } else {
            self.finish_unsaved_exit(exit);
        }
    }

    /// Close the focused group's tab at `idx`: at once when it holds nothing
    /// unsaved, through the prompt when it does.
    pub(super) fn request_close_tab(&mut self, idx: usize) {
        let path = self.editor.tab_path(idx);
        self.guard_unsaved(UnsavedExit::CloseTab { idx, path });
    }

    /// Carry `exit` out, with nothing left to ask.
    pub(super) fn finish_unsaved_exit(&mut self, exit: UnsavedExit) {
        match exit {
            // A quit that goes ahead kept every edit or was told to drop
            // them, so the hot-exit backup goes with it.
            UnsavedExit::Quit => {
                self.clear_hot_exit();
                self.quit = true;
            }
            UnsavedExit::DropToLocal => {
                self.clear_hot_exit();
                self.drop_to_local = true;
                self.quit = true;
            }
            UnsavedExit::RemoteLaunch { host, path } => {
                // What the answer left unsaved (D) goes without a second
                // question at the handoff; anything edited after it does not.
                self.remote_handoff_ok = self.unsaved_stamp();
                self.start_remote_launch(host, path);
            }
            UnsavedExit::RemoteHandoff { .. } => {
                self.clear_hot_exit();
                self.quit = true;
            }
            UnsavedExit::CloseTab { idx, .. } => self.close_tab_now(idx),
        }
    }

    /// Answer the unsaved-changes prompt (#862). `save` writes the buffers
    /// first and goes on only once every one of them is on disk: a file left
    /// unsaved keeps croft (or the tab) open and is named in the status
    /// line. Otherwise it goes on without saving.
    pub(super) fn answer_unsaved(&mut self, save: bool) {
        let Some(exit) = self.pending_unsaved.take() else {
            return;
        };
        if let UnsavedExit::CloseTab { idx, path } = &exit
            && self.editor.tab_path(*idx) != *path
        {
            self.status = String::from("The tab moved while croft was asking; nothing was closed");
            return;
        }
        if save {
            let only = match &exit {
                UnsavedExit::CloseTab { idx, .. } => Some(*idx),
                _ => None,
            };
            let left = self.save_tabs_for_exit(only);
            if !left.is_empty() {
                self.drop_unsaved_exit(&exit);
                self.status = format!(
                    "Did not {}: still unsaved - {}",
                    exit.verb(),
                    left.join(", ")
                );
                return;
            }
        }
        let closing = matches!(exit, UnsavedExit::CloseTab { .. });
        self.finish_unsaved_exit(exit);
        if closing {
            self.status = String::from(if save {
                "Saved and closed tab"
            } else {
                "Closed tab without saving"
            });
        }
    }

    pub(super) fn cancel_unsaved(&mut self) {
        if let Some(exit) = self.pending_unsaved.take() {
            self.drop_unsaved_exit(&exit);
            self.status = format!("Cancelled: did not {}", exit.verb());
        }
    }

    /// Let go of `exit` when the prompt ends without it going ahead. A
    /// remote handoff is already armed (the launch waits for the quit), and
    /// left armed it would go ahead at the next, unrelated quit.
    pub(super) fn drop_unsaved_exit(&mut self, exit: &UnsavedExit) {
        if matches!(exit, UnsavedExit::RemoteHandoff { .. }) {
            self.remote_launch = None;
            self.tear_down_connect_auth();
        }
    }

    /// Close the focused group's tab at `idx` without asking: the shared end
    /// of every single-tab close (Cmd+W, the tab's close button and context
    /// menu, View: Close Editor, vim `:q`). The tab is recorded for Reopen
    /// Closed Editor first, while it is still in place.
    pub(super) fn close_tab_now(&mut self, idx: usize) {
        self.record_closed_tab_at(idx);
        if self.editor.close_tab(idx) {
            self.sync_open_file_poll_mtime();
            self.status = String::from("Closed tab");
            self.poke_cursor();
            // Closing the focused group's last tab while split closes the
            // group: collapse back to the surviving column.
            self.collapse_split_if_empty();
            // A tab closed without saving takes its text out of the
            // hot-exit backup now (#862 review), not at a later tick: a
            // crash in between would bring back what was discarded.
            if self.hot_exit_written.is_some() && self.unsaved_stamp() != self.hot_exit_written {
                self.write_hot_exit();
            }
        }
    }

    /// Whether closing the focused group's tab at `idx` throws unsaved edits
    /// away (#862). Not when another open tab holds the very same unsaved
    /// text: a symbol tab and its file's tab mirror one buffer (#369), so
    /// closing either keeps the edits in the other.
    pub(super) fn tab_would_lose_edits(&self, idx: usize) -> bool {
        let Some(ed) = self.editor.editors.get(idx).filter(|e| holds_unsaved(e)) else {
            return false;
        };
        !std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .any(|other| {
                !std::ptr::eq(other, ed)
                    && other.dirty
                    && other.path.is_some()
                    && other.path == ed.path
                    && other.lines == ed.lines
            })
    }

    /// Every file a quit would lose (#862), one label each: the dirty tabs of
    /// every editor group, a file open in several tabs (a split, a symbol
    /// tab) counted once, and each untitled buffer holding text on its own.
    pub(super) fn unsaved_files(&self) -> Vec<String> {
        let mut seen: Vec<&Path> = Vec::new();
        let mut labels = Vec::new();
        for ed in std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| holds_unsaved(e))
        {
            match ed.path.as_deref() {
                Some(p) if seen.contains(&p) => {}
                Some(p) => {
                    seen.push(p);
                    labels.push(self.status_path(p));
                }
                None => labels.push(
                    ed.untitled
                        .map_or_else(|| String::from("untitled"), |n| format!("Untitled-{n}")),
                ),
            }
        }
        labels
    }

    /// A fingerprint of every unsaved buffer as it stands (#862): which tabs
    /// hold unsaved text and how far each has been edited, so any edit, save
    /// or close changes it. `None` when nothing is unsaved. Cheap enough for
    /// every tick: it hashes paths and edit counters, never text.
    pub(super) fn unsaved_stamp(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut any = false;
        for ed in std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| holds_unsaved(e) || e.sheet_pending_cell().is_some())
        {
            any = true;
            ed.path.hash(&mut h);
            ed.edit_seq.hash(&mut h);
            // A grid's edits leave the text untouched (#1593), and the cell
            // being typed into is in no cell yet.
            if let Some(view) = &ed.sheet {
                for data in &view.sheets {
                    data.revision.hash(&mut h);
                }
            }
            ed.sheet_pending_cell().hash(&mut h);
        }
        any.then(|| h.finish())
    }

    /// Hot exit's pause after the newest unsaved edit before it backs the
    /// buffers up (#862): typing never waits on a write, and a crash loses
    /// seconds of it.
    pub(super) const HOT_EXIT_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

    /// Keep the hot-exit backup (#862) in step with the unsaved buffers.
    /// Runs every tick, but writes only once they changed since the last
    /// backup and [`Self::HOT_EXIT_DELAY`] has passed since the newest edit,
    /// and removes the backup once nothing is unsaved.
    pub fn tick_hot_exit(&mut self, now: std::time::Instant) {
        if self.hot_exit_dir.is_none() {
            return;
        }
        let stamp = self.unsaved_stamp();
        if stamp == self.hot_exit_written && self.hot_exit_consumed.is_empty() {
            return;
        }
        if self.hot_exit_retry_at.is_some_and(|at| now < at) {
            return;
        }
        let settling = stamp.is_some()
            && self
                .newest_unsaved_edit()
                .is_some_and(|t| now.saturating_duration_since(t) < Self::HOT_EXIT_DELAY);
        if !settling {
            self.write_hot_exit();
        }
    }

    /// When the most recently edited unsaved buffer last changed.
    pub(super) fn newest_unsaved_edit(&self) -> Option<std::time::Instant> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| holds_unsaved(e))
            .filter_map(|e| e.last_edit_at)
            .max()
    }

    /// This croft's hot-exit backup file for its workspace, when it keeps
    /// backups at all.
    pub(super) fn hot_exit_path(&self) -> Option<PathBuf> {
        let dir = self.hot_exit_dir.as_deref()?;
        Some(crate::hot_exit::own_path(dir, self.workspace_root()))
    }

    /// Write the hot-exit backup now (#862): every unsaved text buffer of
    /// every editor group, or no backup at all once there is none. Once it
    /// lands, the backups this croft restored from go (never its own file,
    /// which may carry the same name). A write that fails says so once, is
    /// not counted as written, and is tried again by a later tick. Returns
    /// whether it landed.
    pub(super) fn write_hot_exit(&mut self) -> bool {
        let Some(path) = self.hot_exit_path() else {
            return false;
        };
        let root = self.workspace_root().to_path_buf();
        let stamp = self.unsaved_stamp();
        let backup = self.capture_hot_exit();
        let result = if backup.buffers.is_empty() {
            crate::hot_exit::remove(&path, &root).map_err(|e| {
                format!(
                    "Could not remove the hot-exit backup {} ({e})",
                    path.display()
                )
            })
        } else {
            backup
                .save(&path)
                .map_err(|e| format!("Could not back up unsaved changes ({e:#})"))
        };
        match result {
            Ok(()) => {
                self.hot_exit_written = stamp;
                self.hot_exit_retry_at = None;
                let mut left = Vec::new();
                for consumed in std::mem::take(&mut self.hot_exit_consumed) {
                    if consumed == path {
                        continue;
                    }
                    if let Err(e) = crate::hot_exit::remove(&consumed, &root) {
                        left.push(format!("{} ({e})", consumed.display()));
                        self.hot_exit_unremoved.push(consumed);
                    }
                }
                if !left.is_empty() {
                    self.status = format!(
                        "Could not remove the hot-exit backup {}: it may still hold unsaved text; delete it by hand",
                        left.join(", ")
                    );
                }
                true
            }
            Err(note) => {
                if self.hot_exit_retry_at.is_none() {
                    self.status = note;
                }
                self.hot_exit_retry_at = Some(std::time::Instant::now() + Self::HOT_EXIT_DELAY);
                false
            }
        }
    }

    /// What a hot-exit backup holds (#862): each unsaved tab as the update
    /// relaunch captures it (every group, a file once, untitled buffers
    /// that hold text), with the file's stamp from when the buffer last
    /// matched it. Hex and sheet tabs carry no text and are left out, as
    /// from the relaunch.
    pub(super) fn capture_hot_exit(&self) -> crate::hot_exit::Backup {
        let editors: Vec<&Editor> = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .collect();
        let buffers = self
            .capture_session_state_with(false)
            .tabs
            .into_iter()
            .filter(|t| {
                t.dirty
                    && (!t.sheet_cells.is_empty()
                        || t.unsaved_text
                            .as_deref()
                            .is_some_and(|text| t.path.is_some() || !text.is_empty()))
            })
            .map(|tab| {
                let disk_stamp = tab.path.as_deref().and_then(|p| {
                    editors
                        .iter()
                        .find(|e| {
                            (e.dirty || e.sheet_pending_cell().is_some())
                                && e.path.as_deref() == Some(p)
                        })
                        .and_then(|e| e.disk_stamp())
                });
                crate::hot_exit::Buffer { tab, disk_stamp }
            })
            .collect();
        crate::hot_exit::Backup {
            workspace_root: self.workspace_root().to_path_buf(),
            buffers,
        }
    }

    /// Remove this croft's hot-exit backup and the ones it restored from
    /// (#862): the quit ending it kept every unsaved edit on disk, or was
    /// told to drop them. One that cannot be removed is said, in the status
    /// line and on stderr after the quit, since it may still hold text the
    /// user chose to discard.
    pub(super) fn clear_hot_exit(&mut self) {
        let root = self.workspace_root().to_path_buf();
        let mut left = Vec::new();
        let consumed = std::mem::take(&mut self.hot_exit_consumed);
        let unremoved = std::mem::take(&mut self.hot_exit_unremoved);
        for path in self
            .hot_exit_path()
            .into_iter()
            .chain(consumed)
            .chain(unremoved)
        {
            if let Err(e) = crate::hot_exit::remove(&path, &root) {
                left.push(format!("{} ({e})", path.display()));
            }
        }
        self.hot_exit_written = None;
        self.hot_exit_retry_at = None;
        if !left.is_empty() {
            let note = format!(
                "Could not remove the hot-exit backup {}: it may still hold the unsaved text; delete it by hand",
                left.join(", ")
            );
            self.status = note.clone();
            self.exit_notes.push(note);
        }
    }

    /// Remove this croft's own hot-exit backup, keyed by the workspace it
    /// was written for (#862): a re-root writes it again under the new one
    /// at the next tick, and what it restored from stays until then.
    pub(super) fn remove_own_hot_exit(&mut self) {
        if let Some(path) = self.hot_exit_path() {
            let _ = crate::hot_exit::remove(&path, self.workspace_root());
        }
        self.hot_exit_written = None;
    }

    /// Bring back what hot exit kept of this workspace's unsaved buffers
    /// (#862) when croft last ended without a clean quit: a kill, a crash,
    /// an OOM kill. Each comes back as an unsaved tab with its text and
    /// cursor and nothing is written to its file. A file changed on disk
    /// since is named in the status line and keeps the stamp its edits were
    /// made against, so its first save asks before overwriting the change;
    /// a deleted one comes back as a tab for its path. This croft writes its
    /// own backup of them at once and removes the consumed ones only after
    /// that lands, so a crash straight after the launch, or a write that
    /// fails, cannot lose them either. Backups come newest first, and an
    /// older one of a file already restored (two crofts on the workspace,
    /// both killed) is not restored over the newer: that copy stays in its
    /// backup for a later launch.
    /// A backup that cannot be read is kept and said so, until it is older
    /// than
    /// [`crate::hot_exit::UNREADABLE_KEPT_FOR`]. Called once, at a normal
    /// launch.
    pub fn restore_hot_exit(&mut self) -> bool {
        let Some(dir) = self.hot_exit_dir.clone() else {
            return false;
        };
        let root = self.workspace_root().to_path_buf();
        let mut restored = 0usize;
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut gone: Vec<PathBuf> = Vec::new();
        let mut unreadable: Vec<String> = Vec::new();
        let mut expired = 0usize;
        // The files this launch has filled a tab for. Another backup of one
        // of them, older since the newest come first, is a second croft's
        // unsaved copy (two crofts on the workspace, both killed), and
        // restoring it into that tab would overwrite the newer text. It
        // stays in its backup instead, held for a later launch once the
        // newer copy is saved or closed.
        let mut filled: Vec<PathBuf> = Vec::new();
        let mut doubled: Vec<PathBuf> = Vec::new();
        let mut trimmed: Vec<(PathBuf, crate::hot_exit::Backup)> = Vec::new();
        // A dead croft's backup at this croft's own path (the start time
        // could not be read, so the name is the recycled pid alone) would be
        // restored from, written over by this croft's backup and cut down,
        // all as one file. It is moved aside first; one that cannot be is
        // left alone, and this croft keeps no backup rather than write over
        // it.
        let own = self.hot_exit_path();
        let mut orphans = crate::hot_exit::orphaned(&dir, &root);
        if let Some(own) = own.as_ref().filter(|own| orphans.contains(own)) {
            match crate::hot_exit::move_aside(own) {
                Some(aside) => {
                    for file in orphans.iter_mut().filter(|f| *f == own) {
                        *file = aside.clone();
                    }
                }
                None => {
                    self.hot_exit_dir = None;
                    self.status = format!(
                        "Hot exit is off for this session: a backup is at its own path, {}, and could not be moved aside",
                        own.display()
                    );
                    return false;
                }
            }
        }
        for file in orphans {
            let backup = match crate::hot_exit::Backup::load(&file) {
                Ok(backup) if backup.is_of(&root) => backup,
                // Another workspace's, under a colliding digest.
                Ok(_) => continue,
                // Reported for a week already: past recovery by hand.
                Err(_) if crate::hot_exit::is_expired(&file) => {
                    if crate::hot_exit::remove(&file, &root).is_ok() {
                        expired += 1;
                    }
                    continue;
                }
                Err(_) => {
                    unreadable.push(file.display().to_string());
                    continue;
                }
            };
            let before = restored;
            let mut held: Vec<crate::hot_exit::Buffer> = Vec::new();
            for buffer in &backup.buffers {
                if let Some(path) = buffer.tab.path.as_ref().filter(|p| filled.contains(p)) {
                    doubled.push(path.clone());
                    held.push(buffer.clone());
                    continue;
                }
                let has_cells = buffer.tab.dirty && !buffer.tab.sheet_cells.is_empty();
                let Some((ed, unapplied)) = self.restore_open_tab(&buffer.tab) else {
                    // An xlsx's cells whose workbook did not open (gone,
                    // unreadable, unmounted) are kept for a later launch
                    // rather than dropped with the backup (#1593).
                    if has_cells {
                        held.push(buffer.clone());
                    }
                    continue;
                };
                if !unapplied.is_empty() {
                    // Cells whose sheet or row the workbook no longer has
                    // stay backed up too; a tab none of them reached was
                    // not restored at all.
                    let all = unapplied.len() == buffer.tab.sheet_cells.len();
                    let mut kept = buffer.clone();
                    kept.tab.sheet_cells = unapplied;
                    held.push(kept);
                    if all {
                        continue;
                    }
                }
                restored += 1;
                let Some(path) = buffer.tab.path.clone() else {
                    continue;
                };
                filled.push(path.clone());
                match ed.disk_stamp() {
                    // Kept at the stamp its edits were made against, so a
                    // file made at the path since is not written over
                    // without asking.
                    None => {
                        ed.set_disk_stamp(buffer.disk_stamp);
                        gone.push(path);
                    }
                    Some(now) if buffer.disk_stamp.is_some_and(|then| then != now) => {
                        ed.set_disk_stamp(buffer.disk_stamp);
                        changed.push(path);
                    }
                    Some(_) => {}
                }
            }
            if !held.is_empty() {
                // Not consumed. Once this croft's own backup holds what was
                // restored from it, it is cut down to the held copies, so
                // those do not come back a second time.
                if restored > before {
                    trimmed.push((
                        file,
                        crate::hot_exit::Backup {
                            workspace_root: backup.workspace_root,
                            buffers: held,
                        },
                    ));
                }
            } else if restored == before {
                // Nothing in it to keep (an emptied backup, left by a
                // removal that failed): no write to wait on.
                let _ = crate::hot_exit::remove(&file, &root);
            } else {
                self.hot_exit_consumed.push(file);
            }
        }
        let mut notes = Vec::new();
        if restored > 0 {
            notes.push(format!(
                "Hot exit: restored {restored} unsaved tab{}",
                if restored == 1 { "" } else { "s" }
            ));
            let names = |paths: &[PathBuf]| {
                paths
                    .iter()
                    .map(|p| self.status_path(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if !changed.is_empty() {
                notes.push(format!("{} changed on disk since", names(&changed)));
            }
            if !gone.is_empty() {
                notes.push(format!("{} no longer on disk", names(&gone)));
            }
            if !doubled.is_empty() {
                notes.push(format!(
                    "{} {} kept in {} backup for a later launch",
                    names(&doubled),
                    if doubled.len() == 1 {
                        "has a second unsaved copy,"
                    } else {
                        "have second unsaved copies,"
                    },
                    if doubled.len() == 1 { "its" } else { "their" },
                ));
            }
            let unremoved = self.hot_exit_unremoved.len();
            if self.write_hot_exit() {
                if self.hot_exit_unremoved.len() > unremoved {
                    notes.push(std::mem::take(&mut self.status));
                }
                // A trim that fails leaves the whole backup, so its other
                // files come back again later rather than the held copy
                // going.
                for (file, held) in trimmed {
                    let _ = held.save(&file);
                }
            } else {
                notes.push(format!(
                    "{}; the backup they came from is kept until it can",
                    std::mem::take(&mut self.status)
                ));
            }
        }
        if expired > 0 {
            notes.push(format!(
                "Hot exit: removed {expired} unreadable backup{} older than a week",
                if expired == 1 { "" } else { "s" }
            ));
        }
        if !unreadable.is_empty() {
            notes.push(format!(
                "Hot exit: could not read {} (kept for a week)",
                unreadable.join(", ")
            ));
        }
        if !notes.is_empty() {
            self.status = notes.join("; ");
        }
        restored > 0
    }

    /// Write what an unsaved-changes prompt (#862) asked about: the focused
    /// group's tab at `only`, or every unsaved tab in every group.
    ///
    /// Each write is the explicit save's, done now and unformatted (the tab
    /// is about to go, and a formatter's reply would land after it), and it
    /// refuses what a first Cmd+S refuses: a file changed on disk, text its
    /// encoding cannot hold. A merge with conflicts still open is not written
    /// blind either, and an untitled buffer has no file to go to. Returns a
    /// line per file still unsaved; empty means everything is on disk.
    pub(super) fn save_tabs_for_exit(&mut self, only: Option<usize>) -> Vec<String> {
        let cleanup = self.save_cleanup;
        use crate::widgets::editor::SaveOutcome;
        // The tabs of a file with a symbol tab mirror one text (#369): the
        // first write saves them all, and writing a second would read the
        // first as a change on disk. The sync afterwards marks them clean.
        let mirrored: Vec<PathBuf> = self
            .symbol_view_paths()
            .into_iter()
            .filter(|p| !self.symbol_path_is_live(p))
            .collect();
        // A collab guest never writes a shared file, and loses nothing by
        // leaving it: its edits are already live with the owner, who saves.
        let guest = self.is_collab_guest();
        let root = self.tree.root.clone();
        let collab = &self.collab;
        let owner_saves = |p: &Path| {
            guest
                && collab_file_key(&root, p)
                    .is_some_and(|f| collab.as_ref().is_some_and(|s| !s.is_local_only(&f)))
        };
        // A file open in more than one editor group is a buffer per group.
        // The same unsaved text in each is written once and the other copies
        // marked saved: a second write would read the first as a change on
        // disk. Different texts are not settled by writing whichever group
        // comes first, so neither is written. Each path with more than one
        // unsaved copy, with how many and whether they hold one text.
        let mut copies: Vec<(PathBuf, usize, bool)> = Vec::new();
        if only.is_none() {
            let mut seen: Vec<(PathBuf, &Editor)> = Vec::new();
            for ed in std::iter::once(&self.editor)
                .chain(self.editor_layout.inactive_groups())
                .flat_map(|g| g.editors.iter())
                .filter(|e| holds_unsaved(e) && e.hex.is_none() && e.sheet.is_none())
            {
                let Some(path) = ed.path.clone() else {
                    continue;
                };
                match seen.iter().find(|(p, _)| *p == path) {
                    Some((_, first)) => {
                        let same = first.lines == ed.lines
                            && first.bytes_for_disk() == ed.bytes_for_disk();
                        match copies.iter_mut().find(|(p, ..)| *p == path) {
                            Some((_, n, one_text)) => {
                                *n += 1;
                                *one_text &= same;
                            }
                            None => copies.push((path, 2, same)),
                        }
                    }
                    None => seen.push((path, ed)),
                }
            }
        }
        let mut saved: Vec<(PathBuf, crate::provenance::Provenance, Option<Vec<u8>>)> = Vec::new();
        // The saved text of each, for the language server's didSave (#854).
        let mut saved_texts: Vec<String> = Vec::new();
        let mut left: Vec<(Option<PathBuf>, String)> = Vec::new();
        let mut save = |ed: &mut Editor| {
            if !holds_unsaved(ed) {
                return;
            }
            let Some(path) = ed.path.clone() else {
                left.push((None, String::from("no file to save to")));
                return;
            };
            if owner_saves(&path)
                || (mirrored.contains(&path) && saved.iter().any(|(q, ..)| *q == path))
            {
                return;
            }
            let reported = left.iter().any(|(q, _)| q.as_ref() == Some(&path));
            match copies.iter().find(|(p, ..)| *p == path) {
                Some((_, n, false)) => {
                    if !reported {
                        left.push((
                            Some(path),
                            format!("open in {n} editor groups with different unsaved text"),
                        ));
                    }
                    return;
                }
                Some((_, _, true)) if saved.iter().any(|(q, ..)| *q == path) => {
                    ed.mark_clean_like_sibling();
                    return;
                }
                // The first copy's save was refused; this one would be too.
                Some((_, _, true)) if reported => return,
                _ => {}
            }
            if ed.merge.as_ref().is_some_and(|m| m.unresolved_count() > 0) {
                left.push((Some(path), String::from("merge conflicts left")));
                return;
            }
            let outcome = if ed.hex.is_some() {
                ed.hex_save(false)
            } else if ed.sheet.is_some() {
                ed.sheet_save(false, false)
            } else {
                ed.save_cleanup = cleanup;
                ed.save_to_disk()
            };
            let why = match outcome {
                Ok(SaveOutcome::Saved) if !ed.dirty => {
                    saved.push((path, ed.provenance_to_record(), ed.bytes_for_disk()));
                    saved_texts.push(ed.document_text());
                    return;
                }
                // A workbook save that held formula cells back names them.
                Ok(SaveOutcome::Saved) => ed.status.clone(),
                Ok(SaveOutcome::DiskConflict) => String::from("changed on disk"),
                Ok(SaveOutcome::EncodingLoss) => {
                    format!("{} cannot hold every character", ed.encoding.name())
                }
                Ok(SaveOutcome::WriteProtected) => String::from("write-protected"),
                Err(e) => e.to_string(),
            };
            left.push((Some(path), why));
        };
        match only {
            Some(idx) => {
                if let Some(ed) = self.editor.editors.get_mut(idx) {
                    save(ed);
                }
            }
            None => {
                self.editor.editors.iter_mut().for_each(&mut save);
                for group in self.editor_layout.inactive_groups_mut() {
                    group.editors.iter_mut().for_each(&mut save);
                }
            }
        }
        if only.is_none() && !saved.is_empty() {
            self.sync_symbol_views();
        }
        // The same follow-ups as every other save path.
        for (path, ..) in &saved {
            self.reload_config_for_path(path);
        }
        if let Some(lsp) = self.lsp.as_ref() {
            for ((path, ..), text) in saved.iter().zip(saved_texts) {
                lsp.save_doc(path.clone(), text);
            }
        }
        for (path, seats, described) in saved {
            self.record_history_snapshot_of(&path, seats, described);
        }
        left.into_iter()
            .map(|(path, why)| match path {
                Some(p) => format!("{} ({why})", self.status_path(&p)),
                None => format!("untitled ({why})"),
            })
            .collect()
    }

    /// File: Revert File (#1285): drop the active tab's unsaved edits for
    /// the text on disk. One undo step, so Ctrl+Z brings the edits back.
    pub(super) fn revert_file(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("This tab has no file to revert to");
            return;
        };
        let name = self.status_path(&path);
        if !self.editor.dirty {
            self.status = format!("{name} has no unsaved changes");
            return;
        }
        match self.editor.revert_as_undo_step() {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.status = format!("Reverted {name}");
            }
            Err(e) => self.status = format!("Revert failed: {e}"),
        }
    }

    /// File: New Untitled Text File (#1580): a scratch tab with no path,
    /// focused; saving it asks where to write it.
    pub(super) fn new_untitled_file(&mut self) {
        // The lowest number no tab holds in any split group: each group
        // numbering its own would give two tabs the same name.
        let taken: Vec<usize> = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|group| group.editors.iter().filter_map(|e| e.untitled))
            .collect();
        let n = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
        self.editor.new_untitled(n);
        self.focus_pane(Pane::Editor);
        self.status = format!("Untitled-{n}: saving asks where to write it");
    }

    /// File: Save As… (#1203): ask where to write the active tab's buffer.
    pub(super) fn open_save_as_prompt(&mut self) {
        if self.editor.has_non_text_view() {
            self.status = String::from("Save As works on text tabs; this one is a preview");
            return;
        }
        let buffer = self
            .editor
            .path
            .as_deref()
            .map(|p| self.status_path(p))
            .unwrap_or_default();
        self.prompt = Some(Prompt {
            label: String::from("Save As"),
            buffer,
            caret: usize::MAX,
            kind: PromptKind::SaveAs,
            target_dir: self.tree.root.clone(),
            error: None,
        });
    }

    /// Write the active tab's buffer to `path` and move the tab there; the
    /// error says why not, with the tab left as it was.
    pub(super) fn save_as(&mut self, path: &Path) -> Result<(), String> {
        use crate::widgets::editor::SaveOutcome;
        self.editor.save_cleanup = self.save_cleanup;
        match self.editor.save_as(path) {
            Ok(SaveOutcome::Saved) => {
                self.status = self.editor.status.clone();
                let seats = self.editor.provenance_to_record();
                let described = self.editor.bytes_for_disk();
                self.record_history_snapshot_of(path, seats, described);
                self.sync_open_file_poll_mtime();
                if let Some(idx) = path.parent().and_then(|d| self.tree.index_of_dir(d)) {
                    self.tree.refresh_children(idx);
                }
                Ok(())
            }
            Ok(SaveOutcome::EncodingLoss) => Err(format!(
                "Some characters can't be written as {}",
                self.editor.encoding.name()
            )),
            Ok(SaveOutcome::WriteProtected) => Err(format!(
                "{} is write-protected: choose another name",
                path.display()
            )),
            Ok(SaveOutcome::DiskConflict) => unreachable!("save_as never checks for a conflict"),
            Err(e) => Err(format!("Save failed: {e}")),
        }
    }
}

/// How long a format-on-save waits for the formatter before writing the
/// file unformatted (#1491).
pub(super) const FORMAT_ON_SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Save All's status line: `before` editors were unsaved, `left` still are,
/// `pending` names the file whose write waits on its formatter, and
/// `active_refusal` is the active tab's own reason it is still unsaved.
pub(super) fn save_all_summary(
    before: usize,
    left: usize,
    pending: Option<&str>,
    active_refusal: String,
) -> String {
    let editors = |n: usize| format!("{n} editor{}", if n == 1 { "" } else { "s" });
    if left == 0
        && let Some(file) = pending
    {
        // Its write waits on the formatter (at most FORMAT_ON_SAVE_TIMEOUT):
        // not saved yet, so not counted as saved (#1491).
        match before.saturating_sub(1) {
            0 => format!("Formatting {file} before saving it"),
            saved => format!(
                "Saved {}; formatting {file} before saving it",
                editors(saved)
            ),
        }
    } else if left == 0 {
        format!("Saved {}", editors(before))
    } else if !active_refusal.is_empty() && pending.is_none() {
        // The active tab's own refusal names the reason and the key
        // that consents; a summary would bury it.
        active_refusal
    } else {
        format!(
            "Save All: {left} editor{} still unsaved - open {} and press Cmd+S",
            if left == 1 { "" } else { "s" },
            if left == 1 { "it" } else { "each" }
        )
    }
}

/// The directory this croft keeps hot-exit backups and sticky notes under
/// (#1578): `state`, after moving in what an earlier croft kept under
/// `cache`, which cache cleaners are free to delete. Run at every launch,
/// so a backup an older croft still writes to the cache is taken over too,
/// once that croft is gone (its own exit removes only the cache copy).
/// Notes are merged note by note into the state store, since an older
/// window may still be adding to its cache copy; lock files stay.
pub(crate) fn user_data_dir(cache: &Path, state: &Path) -> PathBuf {
    crate::hot_exit::move_tree(
        &cache.join("hot-exit"),
        &state.join("hot-exit"),
        &crate::hot_exit::owned_by_a_running_croft,
    );
    crate::sticky_notes::merge_stores(&cache.join("notes"), &state.join("notes"));
    crate::hot_exit::move_tree(&cache.join("notes"), &state.join("notes"), &|p| {
        p.extension().is_some_and(|x| x == "lock")
    });
    state.to_path_buf()
}

pub(super) fn sidebar_view_label(view: SidebarView) -> &'static str {
    match view {
        SidebarView::Explorer => "Explorer",
        SidebarView::Search => "Search",
        SidebarView::SourceControl => "SourceControl",
        SidebarView::Remote => "Remote",
        SidebarView::RunDebug => "RunDebug",
        SidebarView::Extensions => "Extensions",
        SidebarView::Testing => "Testing",
        SidebarView::CodeQL => "CodeQL",
    }
}

pub(super) fn sidebar_view_from_label(label: &str) -> Option<SidebarView> {
    match label {
        "Explorer" => Some(SidebarView::Explorer),
        "Search" => Some(SidebarView::Search),
        "SourceControl" => Some(SidebarView::SourceControl),
        "Remote" => Some(SidebarView::Remote),
        "RunDebug" => Some(SidebarView::RunDebug),
        "Extensions" => Some(SidebarView::Extensions),
        "Testing" => Some(SidebarView::Testing),
        "CodeQL" => Some(SidebarView::CodeQL),
        _ => None,
    }
}

/// The shell line a sudo save runs (#1379): `tee` copies the staged bytes
/// into `target` as root, writing in place so the file keeps its owner and
/// mode, and the staged copy is removed whether or not that worked.
pub(super) fn sudo_save_command(target: &str, staged: &str) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    format!(
        "sudo tee -- {} < {} > /dev/null; rm -f -- {}",
        q(target),
        q(staged),
        q(staged)
    )
}

/// Stage the bytes a sudo save writes (#1379) in a fresh file only this
/// user can read, since they may be a protected file's contents.
pub(super) fn stage_sudo_save(bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let dir = std::env::temp_dir();
    let mut last = None;
    for seq in 0..16 {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!(
            "croft-sudo-save-{}-{seq}-{nonce:08x}",
            std::process::id()
        ));
        match opts.open(&path) {
            Ok(mut file) => {
                return match file.write_all(bytes).and_then(|()| file.flush()) {
                    Ok(()) => Ok(path),
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        Err(e)
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free staging name")))
}
