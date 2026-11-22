//! The file tree (keys, type-to-jump, cut, copy and paste, rename and move with server
//! fixes, delete, drag targets), Explorer sections, workspace folders and
//! .code-workspace files, re-rooting, file watching and outside changes, zoxide jump.

use super::*;

/// The toggleable sub-views stacked inside the Explorer side panel, mirroring
/// VS Code's "Views and More Actions" (⋯) menu on the EXPLORER header. Ordered
/// top-to-bottom exactly as they stack in the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExplorerView {
    OpenEditors,
    Folders,
    Outline,
    Timeline,
    Dependencies,
    AgentLane,
}

impl ExplorerView {
    /// Every view, in stacking order — the order the ⋯ menu lists them.
    pub const ALL: [ExplorerView; 6] = [
        ExplorerView::OpenEditors,
        ExplorerView::Folders,
        ExplorerView::Outline,
        ExplorerView::Timeline,
        ExplorerView::Dependencies,
        ExplorerView::AgentLane,
    ];

    /// Menu / section-header label, matching VS Code's wording. Dependencies is
    /// a static fallback only — its real label is language-aware and computed
    /// from the detected ecosystems (see [`crate::widgets::dependencies`]).
    pub fn label(self) -> &'static str {
        match self {
            ExplorerView::OpenEditors => "Open Editors",
            ExplorerView::Folders => "Folders",
            ExplorerView::Outline => "Outline",
            ExplorerView::Timeline => "Timeline",
            ExplorerView::Dependencies => "Dependencies",
            ExplorerView::AgentLane => "Agent Lane",
        }
    }
}

impl ExplorerViewVisibility {
    pub(super) fn from_prefs(flags: crate::prefs::ExplorerViewsPrefs) -> Self {
        Self { flags }
    }

    pub(super) fn as_prefs(self) -> crate::prefs::ExplorerViewsPrefs {
        self.flags
    }

    pub fn is_visible(&self, view: ExplorerView) -> bool {
        match view {
            ExplorerView::OpenEditors => self.flags.open_editors,
            ExplorerView::Folders => self.flags.folders,
            ExplorerView::Outline => self.flags.outline,
            ExplorerView::Timeline => self.flags.timeline,
            ExplorerView::Dependencies => self.flags.dependencies,
            ExplorerView::AgentLane => self.flags.agent_lane,
        }
    }

    /// Flip one view's visibility, returning the new state.
    pub fn toggle(&mut self, view: ExplorerView) -> bool {
        let slot = match view {
            ExplorerView::OpenEditors => &mut self.flags.open_editors,
            ExplorerView::Folders => &mut self.flags.folders,
            ExplorerView::Outline => &mut self.flags.outline,
            ExplorerView::Timeline => &mut self.flags.timeline,
            ExplorerView::Dependencies => &mut self.flags.dependencies,
            ExplorerView::AgentLane => &mut self.flags.agent_lane,
        };
        *slot = !*slot;
        *slot
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ExplorerClipMode {
    Copy,
    Cut,
}

/// An Explorer rename or move, held while the language servers are asked
/// for their `willRenameFiles` edit (#610).
#[derive(Clone, Debug)]
pub(super) enum FileMove {
    Rename {
        parent: PathBuf,
        old: PathBuf,
        new_name: String,
    },
    Paste {
        dest_dir: PathBuf,
        paths: Vec<PathBuf>,
    },
}

/// The longest an Explorer move waits for the servers before it goes
/// ahead without their edit. Just past the manager's per-server timeout,
/// so a slow server's answer still lands when it can.
pub(super) const FILE_MOVE_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2000);

/// Inter-keystroke gap that keeps the typeahead buffer alive. Anything
/// longer than this resets the prefix to a single character and cycles
/// past the current selection so repeating the same letter walks
/// through matches.
pub(super) const TREE_TYPEAHEAD_WINDOW: std::time::Duration = std::time::Duration::from_millis(750);

/// Build the right-click context-menu items for the explorer.
///
/// * Right-click on an entry (file or non-root folder) → entry-scoped
///   actions: Cut, Copy, Paste, Rename, Delete. Multi-select promotes
///   Delete to a count and keeps Rename on a single entry only.
/// * Right-click on empty tree space, or on the workspace root row →
///   workspace-scoped actions: New File, New Folder, Paste.
///
/// Test-side entry with no viewer lookup, so a viewer installed on the host
/// can never reach the built-in rows these tests assert; the app itself goes
/// through [`App::tree_context_menu_items`] with the dir and the disabled set
/// it carries.
#[cfg(test)]
pub(super) fn build_tree_context_menu_items(
    node: Option<&crate::widgets::file_tree::Node>,
    root: &Path,
    selection: &[PathBuf],
    target_dir: &Path,
    clipboard: Option<&ExplorerClipboard>,
    compare_anchor: Option<&Path>,
) -> Vec<(String, MenuAction)> {
    // No viewer lookup at all: these tests assert the built-in rows, so the
    // lookup must find nothing whatever the host has installed.
    build_tree_context_menu_items_with(
        node,
        root,
        selection,
        target_dir,
        clipboard,
        compare_anchor,
        |_| None,
    )
}

/// [`build_tree_context_menu_items`] with the viewer lookup injected (#465):
/// `viewer_for` answers "which installed viewer handles this file", as
/// `(label, id)`, so a test can offer one without an extensions dir.
pub(super) fn build_tree_context_menu_items_with(
    node: Option<&crate::widgets::file_tree::Node>,
    root: &Path,
    selection: &[PathBuf],
    target_dir: &Path,
    clipboard: Option<&ExplorerClipboard>,
    compare_anchor: Option<&Path>,
    viewer_for: impl Fn(&Path) -> Option<(String, String)>,
) -> Vec<(String, MenuAction)> {
    let entry_target = crate::widgets::file_tree::delete_target_for(node, root);
    let mut items: Vec<(String, MenuAction)> = Vec::new();
    if let Some(p) = entry_target {
        // Right-click on any node (file or folder, nested or not):
        // surface New File… / New Folder… at the top so the user can
        // create a new entry next to / inside whatever they clicked,
        // without having to scroll up to the workspace root first.
        // `target_dir` is already routed by `create_target_dir_for`:
        // a folder click resolves to the folder itself, a file click
        // to the file's parent — same convention VS Code uses.
        items.push((
            String::from("New File"),
            MenuAction::Create(CreateKind::File),
        ));
        items.push((
            String::from("New Folder"),
            MenuAction::Create(CreateKind::Folder),
        ));
        let paths_for_action: Vec<PathBuf> = if selection.iter().any(|sp| sp == &p) {
            selection.to_vec()
        } else {
            vec![p.clone()]
        };
        items.push((
            String::from("Cut"),
            MenuAction::Cut(paths_for_action.clone()),
        ));
        items.push((
            String::from("Copy"),
            MenuAction::Copy(paths_for_action.clone()),
        ));
        if clipboard.is_some() {
            items.push((
                String::from("Paste"),
                MenuAction::Paste(target_dir.to_path_buf()),
            ));
        }
        if paths_for_action.len() == 1 {
            items.push((String::from("Rename"), MenuAction::Rename(p.clone())));
        }
        // Compare actions only make sense for a single regular file.
        let single_file_target = paths_for_action
            .first()
            .filter(|_| paths_for_action.len() == 1)
            .filter(|pp| pp.is_file());
        if let Some(file) = single_file_target {
            // An installed viewer for this file kind (#465): `Open in csvlens`
            // on a .csv, once the csvlens extension has been added.
            if let Some((label, id)) = viewer_for(file) {
                items.push((label, MenuAction::OpenInViewer(id, file.clone())));
            }
            match compare_anchor {
                Some(anchor) if anchor != file.as_path() => {
                    items.push((
                        format!(
                            "Compare with Selected ({})",
                            anchor
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| anchor.display().to_string()),
                        ),
                        MenuAction::CompareWithSelected {
                            anchor: anchor.to_path_buf(),
                            other: file.clone(),
                        },
                    ));
                }
                _ => {}
            }
            items.push((
                String::from("Select for Compare"),
                MenuAction::SelectForCompare(file.clone()),
            ));
        }
        // Two regular files selected: offer a direct diff between them,
        // skipping the two-step Select-for-Compare anchor dance. Folders
        // can't be diffed, so any folder in the selection suppresses it.
        if paths_for_action.len() == 2 && paths_for_action.iter().all(|p| p.is_file()) {
            items.push((
                String::from("Compare Selected"),
                MenuAction::CompareWithSelected {
                    anchor: paths_for_action[0].clone(),
                    other: paths_for_action[1].clone(),
                },
            ));
        }
        // "Make root" appears only for non-root folders. Re-roots the
        // workspace at the clicked folder so the user can dive into a
        // nested git repo without relaunching croft.
        let single_folder_target: Option<&PathBuf> = paths_for_action
            .first()
            .filter(|_| paths_for_action.len() == 1)
            .filter(|_| node.map(|n| n.is_dir).unwrap_or(false))
            .filter(|pp| pp.as_path() != root);
        if let Some(folder) = single_folder_target {
            items.push((
                String::from("Make root"),
                MenuAction::MakeRoot(folder.clone()),
            ));
        }
        let label = if paths_for_action.len() > 1 {
            format!("Delete {} items", paths_for_action.len())
        } else {
            String::from("Delete")
        };
        items.push((
            label,
            MenuAction::Delete {
                paths: paths_for_action,
            },
        ));
    } else {
        items.push((
            String::from("New File"),
            MenuAction::Create(CreateKind::File),
        ));
        items.push((
            String::from("New Folder"),
            MenuAction::Create(CreateKind::Folder),
        ));
        // Root section rows (multi-root, #147): grow the folder set from
        // any root row; drop a SECONDARY root from its own row. The
        // primary is the launch identity — Make Root changes it, never a
        // removal.
        if let Some(n) = node.filter(|n| n.depth == 0) {
            items.push((
                String::from("Add Folder to Workspace"),
                MenuAction::AddWorkspaceFolder,
            ));
            if n.path != root {
                items.push((
                    String::from("Remove Folder from Workspace"),
                    MenuAction::RemoveWorkspaceFolder(n.path.clone()),
                ));
            }
        }
        if clipboard.is_some() {
            items.push((
                String::from("Paste"),
                MenuAction::Paste(target_dir.to_path_buf()),
            ));
        }
    }
    items
}

/// Insert "Reveal in Finder" into a freshly-built tree context menu, but only
/// when the action can actually do something: `enabled` is true (a local macOS
/// session) and a concrete entry was clicked (`clicked`). On a remote SSH
/// session croft runs on the headless host where there is no Finder, so the
/// item is omitted entirely rather than shown as a dead no-op, keeping the
/// local and remote menus honest. The entry lands directly beneath the New
/// File / New Folder block to mirror VS Code's top "reveal" placement.
pub(super) fn maybe_add_reveal_in_finder(
    items: &mut Vec<(String, MenuAction)>,
    clicked: Option<&Path>,
    enabled: bool,
) {
    let Some(path) = clicked.filter(|_| enabled) else {
        return;
    };
    // Insert directly after the leading New File / New Folder block: find the
    // first non-Create entry and slot in ahead of it. Deriving the position
    // this way (rather than a literal index) keeps the placement correct even
    // if the Create block grows or the empty-area menu shape changes.
    let pos = items
        .iter()
        .position(|(_, a)| !matches!(a, MenuAction::Create(_)))
        .unwrap_or(items.len());
    items.insert(
        pos,
        (
            String::from("Reveal in Finder"),
            MenuAction::RevealInFinder(path.to_path_buf()),
        ),
    );
}

/// Minimum width of the zoxide jump popup. The Explorer column it anchors
/// over is typically ~32 cells, too narrow to read frecency paths like
/// `~/Documents/platform/app/...`, so the box widens rightward to at least
/// this many cells (capped to the frame) while staying left-anchored on
/// the Explorer.
pub(super) const ZOXIDE_JUMP_MIN_WIDTH: u16 = 56;

/// The status-bar line for a jump popup with no zoxide to run: the first
/// line of what the popup body says.
pub(super) fn zoxide_jump_status(jump: &crate::widgets::zoxide_jump::ZoxideJump) -> String {
    let msg = crate::widgets::zoxide_jump::unavailable_message(jump);
    format!("Jump: {}", msg.lines().next().unwrap_or_default())
}

impl App {
    /// The Explorer's right-click menu (see [`build_tree_context_menu_items`]),
    /// with the viewer row read from the extensions under the app's config
    /// dir and its own disabled set, so the row it builds and the click that
    /// resolves the row read the same extensions, on or off as the
    /// Extensions panel shows them (#2076).
    pub(super) fn tree_context_menu_items(
        &self,
        node: Option<&crate::widgets::file_tree::Node>,
        selection: &[PathBuf],
        target_dir: &Path,
    ) -> Vec<(String, MenuAction)> {
        build_tree_context_menu_items_with(
            node,
            &self.tree.root,
            selection,
            target_dir,
            self.tree_clipboard.as_ref(),
            self.compare_anchor.as_deref(),
            |file| {
                crate::mcp::registry::viewer_for_path_in_dir(
                    &self.config_dir,
                    &self.disabled_extensions,
                    file,
                )
                .map(|v| (v.label.clone(), v.key()))
            },
        )
    }

    /// What the poll stats for open tabs: each tab's file, and for a SQLite
    /// tab its write-ahead log too, where a WAL-mode app's commits land
    /// while the database file stays untouched (#1643). The log may not
    /// exist yet; its appearing is a change like any other.
    pub(super) fn polled_open_file_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.open_tab_paths();
        let wal = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| {
                e.sheet
                    .as_ref()
                    .is_some_and(|s| s.kind == crate::sheet::SheetKind::Sqlite)
            })
            .filter_map(|e| {
                e.path
                    .as_deref()
                    .and_then(crate::widgets::editor::sqlite_wal_path)
            });
        paths.extend(wal);
        paths
    }

    /// Every file backing an open tab, across all editor groups. The poll
    /// stats these, so a background tab in a dir that carries no watch still
    /// sees an external write.
    pub(super) fn open_tab_paths(&self) -> Vec<PathBuf> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter_map(|e| e.path.clone())
            .collect()
    }

    /// Sweep EVERY open tab for external on-disk changes, not just the
    /// focused one — in every editor GROUP, not just the focused one either.
    /// The poll stats each open path once and re-baselines its stamp, so a
    /// change reported to a sweep that could not reach the tab is burned: no
    /// later tick reports it again and that pane keeps stale text forever.
    /// Clean tabs reload silently; dirty tabs are flagged as conflicts (the
    /// buffer and the disk are both preserved); tabs whose reload FAILED
    /// keep their last good view and say so in the status line (#37).
    /// Returns true if any tab reloaded, failed, or entered conflict, so
    /// the caller redraws.
    pub(super) fn reload_open_file_after_external_change(&mut self) -> bool {
        let settled = self.settle_sudo_saves();
        let guest = self.is_collab_guest();
        let root = self.tree.root.clone();
        let skip = |p: &Path| guest && collab_file_key(&root, p).is_some();
        let mut report = self.editor.reload_externally_changed_tabs(&skip);
        for group in self.editor_layout.inactive_groups_mut() {
            let extra = group.reload_externally_changed_tabs(&skip);
            report.reloaded.extend(extra.reloaded);
            report.conflicts.extend(extra.conflicts);
            report.failed.extend(extra.failed);
            report.deleted.extend(extra.deleted);
        }
        if report.is_empty() {
            return settled;
        }
        // The active file changed on disk under a clean buffer: its coverage
        // marks describe the old text now, so they dim like an edit's (#263).
        if let Some(active) = self.editor.path.as_ref()
            && report.reloaded.contains(active)
            && let Some(lens) = self.editor.coverage.as_mut()
        {
            lens.stale = true;
        }
        self.refresh_git_status_debounced();
        match (
            report.reloaded.len(),
            report.conflicts.len(),
            report.failed.len(),
        ) {
            // A dirty-buffer collision outranks everything: it needs a
            // decision, so it gets the modal.
            (_, c, _) if c > 0 => {
                self.prompt_disk_conflict(report.conflicts.clone());
            }
            // Gone is not a failed read (#900): the tab keeps its text, its
            // label says `(deleted)`, and a save puts the file back.
            // A failed reload in the same sweep is said too: it is reported
            // once, so dropping it here would hide it for good.
            _ if !report.deleted.is_empty() => {
                let deleted = match report.deleted.as_slice() {
                    [one] => format!(
                        "{} was deleted on disk; save to recreate it",
                        self.status_path(one)
                    ),
                    many => format!(
                        "{} open files were deleted on disk; save to recreate them",
                        many.len()
                    ),
                };
                let failed = match report.failed.as_slice() {
                    [] => String::new(),
                    [one] => format!(
                        "; reload failed for {}, keeping its last good view",
                        self.status_path(one)
                    ),
                    many => format!(
                        "; reload failed for {} files, keeping their last good view",
                        many.len()
                    ),
                };
                self.status = format!("{deleted}{failed}");
            }
            // A failed reload used to be silently discarded (#37): the tab
            // keeps its last good view, and the status must say so instead
            // of letting the user read stale content as current.
            (0, 0, 1) => {
                let p = report.failed[0].display();
                self.status = format!("Reload failed for {p}; keeping the last good view");
            }
            (r, 0, f) if f > 0 => {
                self.status = if r == 0 {
                    format!("Reload failed for {f} files changed on disk")
                } else {
                    format!("Reloaded {r} files, reload failed for {f} (kept their last good view)")
                };
            }
            (1, 0, 0) => {
                let p = report.reloaded[0].display();
                self.status = format!("Reloaded {p} (external change)");
            }
            (n, ..) => {
                self.status = format!("Reloaded {n} files changed on disk");
            }
        }
        // Keep the watcher's active-file baseline aligned with the reload so
        // a just-applied change doesn't re-trigger on the next poll.
        self.sync_open_file_poll_mtime();
        true
    }

    /// A dirty buffer collided with an external write. Surface it unmissably
    /// via the confirm popup (Enter reloads and discards local edits, Esc
    /// keeps them). Falls back to the status line only when another prompt
    /// already owns the modal slot, so we never stomp an in-progress input.
    pub(super) fn prompt_disk_conflict(&mut self, paths: Vec<PathBuf>) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        // It opens mid-typing and takes the keys, so nothing a stray Enter
        // or a typed line can confirm discards the edits: only the word
        // `reload` does, and the box says so (#910).
        let title = match paths.as_slice() {
            [one] => format!(
                "{} changed on disk and you have unsaved edits.",
                self.status_path(one)
            ),
            many => format!(
                "{} open files changed on disk with unsaved edits.",
                many.len()
            ),
        };
        if self.input_prompt.is_some() {
            self.status = title;
            return;
        }
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::ReloadConflict { paths },
                title,
                "type reload to discard your unsaved edits",
            )
            .with_hint("reload + Enter discards your edits · anything else keeps them"),
        );
    }

    pub(super) fn poll_filesystem_changes(&mut self) -> bool {
        // Runs every frame; the poll itself fires at most every 50ms and backs
        // off from there, so don't build the path list until it would be used.
        if !self.fs_watch.poll_due() {
            return false;
        }
        let open_paths = self.polled_open_file_paths();
        let poll = self.fs_watch.poll(&mut self.tree, &open_paths);
        // Any directory change can also be a write to a file open in some
        // tab (background or active), so sweep all tabs whenever the poll
        // saw activity, not only when the active file's own stat moved.
        if poll.open_file_changed || poll.dirs_changed {
            self.reload_open_file_after_external_change();
        }
        if poll.dirs_changed {
            self.refresh_git_status_debounced();
            if poll.finder_relevant {
                self.kick_file_finder_index_rebuild();
            }
        }
        // Stamp-gated, so an idle poll costs a stat per open diff view; on
        // hosts without a native watcher this is how a diff follows the
        // file (#471).
        let diffs_changed =
            self.refresh_open_diff_views(false, &[], &std::collections::BTreeSet::new());
        poll.open_file_changed || poll.dirs_changed || diffs_changed
    }

    /// Drain any pending filesystem events from the watcher and refresh the
    /// tree directories whose contents may have changed. Also reloads the
    /// editor's open file when an external write (vim, git pull, etc.)
    /// changes it on disk.
    /// Drain pending filesystem events. Returns `true` iff anything was
    /// processed (so the main loop knows it owes a redraw).
    /// Install background-initialised resources (fs watcher, git status) if
    /// their threads have finished. Returns true if any were installed this
    /// tick (so the caller redraws). Cheap no-op once both are installed.
    pub fn try_install_pending_init(&mut self) -> bool {
        let mut changed = self.fs_watch.try_install_watcher();
        for w in &mut self.fs_watch_extra {
            if w.try_install_watcher() {
                changed = true;
            }
        }
        if self.drain_git_responses() {
            changed = true;
        }
        // Land the background-built Cmd+P file index the moment the
        // walker thread finishes, so the very next Cmd+P opens
        // against a hot index instead of triggering the synchronous
        // fallback walk.
        self.poll_file_finder_index();
        changed
    }

    /// Keep the Explorer's non-OUTLINE stacked sub-views current: re-project the
    /// OPEN EDITORS list from the tabs, kick off the TIMELINE / DEPENDENCIES
    /// background fetches when their input (active file / workspace root) changed,
    /// and drain any worker replies. Returns whether anything changed, so the
    /// main loop only repaints when it must. All the heavy work (`git log`,
    /// `cargo metadata`) runs on spawned threads — never on this tick.
    pub(super) fn sync_explorer_panels(&mut self) -> bool {
        let mut changed = false;

        // --- OPEN EDITORS: a pure projection of the focused group's tabs. ---
        let active = self.editor.active_index();
        // Same disambiguated titles as the tab strip (#167), so the two
        // views never name one file two ways.
        let labels = self.editor.tab_display_labels();
        let items: Vec<OpenEditorItem> = self
            .editor
            .iter_tabs()
            .enumerate()
            .filter_map(|(i, ed)| {
                ed.path.as_ref().map(|p| OpenEditorItem {
                    path: p.clone(),
                    name: labels
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| p.display().to_string()),
                    dirty: ed.dirty,
                    active: i == active,
                })
            })
            .collect();
        changed |= self.open_editors.set_items(items);

        // --- TIMELINE: git history of the active file, fetched off-thread. ---
        if self.explorer_views.is_visible(ExplorerView::Timeline) {
            match self.editor.path.clone() {
                Some(path) => {
                    if self.timeline_fetched.as_ref() != Some(&path) {
                        self.timeline_fetched = Some(path.clone());
                        self.timeline.begin_loading(path.clone());
                        changed = true;
                        let timeline_root = self
                            .roots
                            .owning_root(&path)
                            .unwrap_or_else(|| self.roots.primary())
                            .to_path_buf();
                        let rel = path
                            .strip_prefix(&timeline_root)
                            .ok()
                            .map(|r| r.to_string_lossy().into_owned());
                        let root = timeline_root;
                        let tx = self.timeline_tx.clone();
                        let history_root = self.history_root.clone();
                        let abs = path.clone();
                        let now = now_millis();
                        std::thread::spawn(move || {
                            // Merge git commits with local snapshots so the
                            // TIMELINE shows both sources, like VS Code. Git
                            // history only exists inside the workspace, but
                            // snapshots are path-keyed and repo-free, so a
                            // file outside the root still lists its versions.
                            let commits = rel
                                .map(|rel| crate::git::file_history(&root, &rel, 50))
                                .unwrap_or_default();
                            let snaps = crate::history::entries_in(&history_root, &abs);
                            let entries = crate::history::merged_timeline(commits, &snaps, now);
                            let _ = tx.send((path, entries));
                        });
                    }
                }
                None => {
                    if self.timeline_fetched.is_some() {
                        self.timeline_fetched = None;
                        self.timeline.clear();
                        changed = true;
                    }
                }
            }
        }
        // A background test-source lookup landed: jump or say it missed. A
        // reply from before an Explorer re-root names the old root; drop it.
        while let Ok((root, name, hit)) = self.test_jump_rx.try_recv() {
            if root != self.workspace_root() {
                continue;
            }
            match hit {
                Some((path, line)) => self.go_to_definition(path, line, 0),
                None => self.status = format!("No source found for test {name}"),
            }
            changed = true;
        }
        // A history snapshot finished writing: refetch the TIMELINE so the
        // new version shows (resetting the marker before the write landed
        // would list the store without it).
        while let Ok(path) = self.history_done_rx.try_recv() {
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.timeline_fetched = None;
                changed = true;
            }
        }
        // The workspace is too large for the sweep to start on its own.
        // Held rather than written: the result below sets `status`
        // unconditionally and nothing paints between the two, so writing it
        // here DESTROYED the hint whenever the checker finished inside one
        // tick - and it is a one-shot, so it never came back. The
        // fast-failure path (over-cap root, no compiler installed) is
        // exactly where that happened.
        let hinted_now = self.project_hint_rx.try_recv().is_ok();
        if hinted_now {
            // Here, not at the spawn: only a hint that was actually WARRANTED
            // spends the one-shot.
            self.project_check_hinted = true;
        }
        // A whole-project check finished: its rows and its verdict land
        // together, so the status cannot describe a different run's result.
        if self.drain_project_check() {
            changed = true;
        }
        if hinted_now {
            // Appended, so the outcome the user asked for still leads.
            self.status.push_str(
                " - too large for the whole-project check to run on its own; \
                 set it to always in Problems: Whole-Project Check Auto-Run",
            );
            changed = true;
        }
        // A CodeQL database's folder size landed; drop one for a database
        // removed while it was being summed.
        while let Ok((path, size)) = self.codeql_size_rx.try_recv() {
            self.codeql_size_pending.remove(&path);
            if let Some(size) = size
                && self.codeql.databases.iter().any(|db| db.path == path)
            {
                self.codeql.db_sizes.insert(path, size);
                changed = true;
            }
        }
        // Drain any TIMELINE replies; ignore one for a since-closed file.
        while let Ok((path, entries)) = self.timeline_rx.try_recv() {
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.timeline.set_history(path, entries);
                changed = true;
            }
        }
        // Collect the COMMITS graph fetch; ignore a reply for a since-left root.
        let settled = match self.graph_fetch.as_ref().map(crate::jobs::Job::poll) {
            None | Some(crate::jobs::Poll::Pending) => None,
            Some(crate::jobs::Poll::Done(reply)) => Some(Some(reply)),
            // Its thread crashed, and the crash notice said so. It is no
            // longer running, so the next refresh fetches again instead of
            // waiting on it until a restart (#2564).
            Some(crate::jobs::Poll::Died) => Some(None),
        };
        if let Some(reply) = settled {
            self.graph_fetch = None;
            if let Some((root, revspec, rows)) = reply
                && root == self.active_scm_root
                && revspec == self.graph_revspec
            {
                self.commit_graph.set_rows(rows);
                changed = true;
            }
            // A trigger arrived while this fetch was in flight: what it
            // asked for (new root, newer HEAD) is not what just landed.
            if self.graph_refetch_queued {
                self.graph_refetch_queued = false;
                self.refresh_commit_graph();
            }
        }

        // --- DEPENDENCIES: detect the workspace's package ecosystems from its
        // manifest files (cheap, once per root), set the language-aware header,
        // then resolve the packages off-thread. The section is gated on
        // detection: a root with no recognised manifest detects nothing, so it
        // is hidden and dropped from the ⋯ menu (the rule every reference editor
        // follows). The fetch only fires when the section is actually shown. ---
        let root = self.tree.root.clone();
        if self.deps_fetched.as_ref() != Some(&root) {
            self.deps_fetched = Some(root.clone());
            self.dep_ecosystems = crate::widgets::dependencies::detect_ecosystems(&root);
            self.dependencies
                .set_header(crate::widgets::dependencies::header_label(
                    &self.dep_ecosystems,
                ));
            changed = true;
            // Whatever ran for the folder just left is dropped here, so its
            // list cannot land in this one's (#2573).
            self.deps_fetch = None;
            if !self.dep_ecosystems.is_empty()
                && self.explorer_views.is_visible(ExplorerView::Dependencies)
            {
                let job =
                    crate::jobs::Job::spawn("dependencies", crate::jobs::Start::Now, move || {
                        crate::widgets::dependencies::fetch_dependencies(&root)
                    });
                match job {
                    Ok(job) => {
                        self.dependencies.lookup_started();
                        self.deps_fetch = Some(job);
                    }
                    Err(e) => {
                        self.dependencies.set_failed();
                        self.status = format!("Could not list the dependencies: {e}");
                    }
                }
            }
        }
        let settled = match self.deps_fetch.as_ref().map(crate::jobs::Job::poll) {
            None | Some(crate::jobs::Poll::Pending) => None,
            Some(crate::jobs::Poll::Done(deps)) => Some(Some(deps)),
            // Its thread crashed, and the crash notice said so. The section
            // says the list could not be made instead of "Loading" until the
            // folder changes (#2572).
            Some(crate::jobs::Poll::Died) => Some(None),
        };
        if let Some(deps) = settled {
            self.deps_fetch = None;
            match deps {
                Some(deps) => self.dependencies.set_deps(deps),
                None => self.dependencies.set_failed(),
            }
            changed = true;
        }

        changed
    }

    /// Activate the editor tab for `path` (clicked in OPEN EDITORS). The path is
    /// already open by definition, so this selects its tab; should it have been
    /// closed between the projection and the click, it re-opens it.
    pub(super) fn activate_open_editor(&mut self, path: PathBuf) {
        if let Some(idx) = self.editor.find_tab_with_path(&path) {
            self.editor.select(idx);
        } else if self.editor.open_pinned(&path).is_err() {
            self.status = format!("Could not open {}", path.display());
            return;
        }
        self.focus_pane(Pane::Editor);
        self.sync_open_file_poll_mtime();
    }

    /// Open the diff a TIMELINE commit introduced for `path` (`git show <hash>
    /// -- <path>`) in a read-only side-by-side editor tab.
    pub(super) fn open_timeline_diff(&mut self, path: PathBuf, hash: String) {
        // Disarm first: every early exit below must leave "Restore Snapshot"
        // pointing at nothing, never at whatever snapshot was viewed before.
        self.history_restore = None;
        let rel = match path.strip_prefix(&self.tree.root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => path.display().to_string(),
        };
        // A local-history row: diff the snapshot against the working file and
        // remember it so "Restore Snapshot" can write it back.
        if let Some(millis) = crate::history::snapshot_millis(&hash) {
            let Some(snap_file) =
                crate::history::snapshot_file_in(&self.history_root, &path, millis)
            else {
                self.status = String::from("Snapshot no longer available");
                return;
            };
            let Ok(content) = std::fs::read(&snap_file) else {
                self.status = String::from("Could not read snapshot");
                return;
            };
            // Snapshots hold the file's raw bytes; decode like `Editor::open`
            // so a UTF-16 file's history diffs as text, not mojibake.
            let enc = encoding_rs::Encoding::for_bom(&content)
                .map(|(e, _)| e)
                .unwrap_or(encoding_rs::UTF_8);
            let text = enc.decode(&content).0.into_owned();
            let label = std::path::PathBuf::from(format!("{rel} (local snapshot)"));
            let unsaved = self.unsaved_text_in_any_group(&path);
            if let Err(e) = self
                .editor
                .open_head_diff_with_text_resolved(label, &text, &path, false, unsaved)
            {
                self.status = format!("Could not open snapshot diff: {e}");
                return;
            }
            self.tag_open_diff(crate::widgets::diff::DiffSource::FixedLeft { left_text: text });
            self.history_restore = Some((path, millis, content));
            self.focus_pane(Pane::Editor);
            self.status = format!("Showing {rel} at a local snapshot");
            return;
        }
        // A git commit row: no snapshot to restore.
        let timeline_root = self
            .editor
            .path
            .as_deref()
            .and_then(|p| self.roots.owning_root(p))
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        // Git: Open Commit for Line passes the full hash (#1556), named here
        // by its first eight digits as the inline blame names it; a TIMELINE
        // row's is git's own short form already.
        let shown = if hash.len() >= 40 {
            hash[..8].to_string()
        } else {
            hash.clone()
        };
        self.status = format!("Loading {rel} at {shown}\u{2026}");
        self.spawn_git_view(move || {
            let result = crate::git::show_commit_file_diff(&timeline_root, &hash, &rel);
            Box::new(move |app: &mut App| match result {
                Ok(raw) => {
                    let label = std::path::PathBuf::from(format!("{rel} @ {shown}"));
                    if let Err(e) = app.editor.open_git_diff_side_by_side(&label, &raw) {
                        app.status = format!("Could not open diff: {e}");
                        return;
                    }
                    app.focus_pane(Pane::Editor);
                    app.status = format!("Showing {rel} at {shown}");
                }
                Err(e) => app.status = format!("git show failed: {e}"),
            })
        });
    }

    /// Restore the local snapshot currently shown in a TIMELINE diff: write its
    /// contents back to the file and reload the buffer. Reachable from the
    /// Command Palette once a snapshot diff is open (which records the target).
    pub(super) fn restore_history_snapshot(&mut self) {
        let Some((file, millis, content)) = self.history_restore.clone() else {
            self.status = String::from("Open a local snapshot from the TIMELINE first");
            return;
        };
        // What is on disk now is kept first: it may never have been
        // snapshotted (an agent's write, a checkout), and the restore
        // overwrites it. Refused if it cannot be kept.
        if let Ok(current) = std::fs::read(&file)
            && let Err(e) =
                crate::history::record_kept_in(&self.history_root, &file, &current, now_millis())
        {
            self.status = format!("Restore cancelled: could not keep the current version ({e})");
            return;
        }
        if let Err(e) = std::fs::write(&file, &content) {
            self.status = format!("Restore failed: {e}");
            return;
        }
        // The snapshot's seats describe exactly the bytes just written (#349),
        // so they come along: onto the buffer, and onto the snapshot this
        // restore records below. No open tab's map can serve instead: a
        // reverted tab has just had its map cleared, and an unreverted one
        // still describes the text the restore replaced. A snapshot with no
        // sidecar restores every line unknown, which is what it knows.
        let seats =
            crate::history::seats_for(&self.history_root, &file, millis).unwrap_or_default();
        // If that file is the active tab (the snapshot diff opened it), reload
        // so the editor shows the restored contents and the disk stamp resyncs.
        // `holds_exactly` re-checks rather than assuming the reload landed the
        // restored bytes: another process can rewrite the file between the
        // write above and the read inside the revert.
        if self.editor.path.as_deref() == Some(file.as_path())
            && self.editor.revert_to_disk().is_ok()
            && self.editor.holds_exactly(&content)
        {
            let mut onto_buffer = seats.clone();
            onto_buffer.truncate(self.editor.lines.len());
            self.editor.provenance = onto_buffer;
        }
        // The restore is itself a new version worth keeping, kept like the
        // version it replaced.
        self.record_history_snapshot_of_kept(&file, seats);
        self.status = format!("Restored {}", self.status_path(&file));
    }

    /// Open the Explorer's "Views and More Actions" (⋯) menu: one row per
    /// stacked sub-view, each prefixed with a check when currently shown.
    /// Selecting a row toggles that view (see [`Self::toggle_explorer_view`]).
    /// The ⋯-menu / status label for a sub-view. DEPENDENCIES is language-aware
    /// ("Rust Dependencies", "Python Dependencies", or plain "Dependencies" for
    /// a polyglot root), computed from the detected ecosystems; every other view
    /// uses its static name.
    pub(super) fn explorer_view_label(&self, view: ExplorerView) -> String {
        match view {
            ExplorerView::Dependencies => {
                crate::widgets::dependencies::menu_label(&self.dep_ecosystems)
            }
            other => other.label().to_string(),
        }
    }

    /// Whether a sub-view may appear in the ⋯ menu and stack in the panel.
    /// DEPENDENCIES is additionally gated on a manifest being detected at the
    /// root, so a plain folder shows no dependency view at all; every other view
    /// is always available.
    pub(super) fn explorer_view_available(&self, view: ExplorerView) -> bool {
        match view {
            ExplorerView::Dependencies => !self.dep_ecosystems.is_empty(),
            _ => true,
        }
    }

    /// Anchored just below the ⋯ button on the EXPLORER title line; the
    /// menu-rect clamp pulls it left so it never spills off the right edge.
    pub(super) fn open_explorer_views_menu(&mut self) {
        let btn = self.tree.header_views_btn;
        let origin = if btn.width > 0 {
            (btn.x, btn.y + 1)
        } else {
            (self.tree.last_area.x + 1, self.tree.last_area.y + 1)
        };
        let items: Vec<(String, MenuAction)> = ExplorerView::ALL
            .iter()
            .filter(|&&view| self.explorer_view_available(view))
            .map(|&view| {
                let mark = if self.explorer_views.is_visible(view) {
                    "\u{2713} "
                } else {
                    "  "
                };
                (
                    format!("{mark}{}", self.explorer_view_label(view)),
                    MenuAction::ToggleExplorerView(view),
                )
            })
            .collect();
        self.context_menu = Some(ContextMenu::flat(
            origin,
            items,
            self.workspace_root().to_path_buf(),
        ));
    }

    /// Flip one Explorer sub-view's visibility, persist the new set, and (for a
    /// data-backed view turned on) clear its fetch latch so the next frame
    /// re-populates it. The layout reads `explorer_views` directly, so no other
    /// wiring is needed for the section to appear or vanish.
    pub(super) fn toggle_explorer_view(&mut self, view: ExplorerView) {
        let now_visible = self.explorer_views.toggle(view);
        if now_visible {
            match view {
                ExplorerView::Timeline => self.timeline_fetched = None,
                ExplorerView::Dependencies => self.deps_fetched = None,
                _ => {}
            }
        }
        self.status = format!(
            "{} {}",
            if now_visible { "Showing" } else { "Hiding" },
            self.explorer_view_label(view)
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_explorer_views(
                self.explorer_views.as_prefs(),
            ));
        }
    }

    /// The AGENT LANE rows (#345): each agent that changed files, then those
    /// files as the ledger orders them (unreviewed first), labelled relative
    /// to their workspace root.
    pub(super) fn agent_lane_panel_rows(&mut self) -> Vec<crate::widgets::agent_lane::LaneRow> {
        use crate::widgets::agent_lane::LaneRow;
        let mut rows = Vec::new();
        let agents: Vec<String> = self
            .agent_ledger
            .agents()
            .into_iter()
            .map(str::to_string)
            .collect();
        for agent in agents {
            rows.push(LaneRow::Agent {
                name: agent.clone(),
                unreviewed: self.agent_ledger.unreviewed_count(&agent),
            });
            let files: Vec<_> = self
                .agent_ledger
                .lane(&agent)
                .into_iter()
                .map(|f| {
                    (
                        f.path.clone(),
                        f.reviewed_millis,
                        f.current_hash,
                        f.unreviewed(),
                    )
                })
                .collect();
            for (path, reviewed_millis, current_hash, unreviewed) in files {
                let changes = reviewed_millis
                    .and_then(|millis| self.lane_changes(&path, millis, current_hash));
                let label = self
                    .roots
                    .owning_root(&path)
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                rows.push(LaneRow::File {
                    agent: agent.clone(),
                    path,
                    label,
                    unreviewed,
                    changes,
                });
            }
        }
        rows
    }

    /// Lines added and removed in `path` since the snapshot it was reviewed
    /// against (#345), for its lane row. Rows render every frame, so the
    /// count is kept until the snapshot, the agent's last write or the file
    /// on disk changes. `None` when the snapshot or the file can't be read.
    pub(super) fn lane_changes(
        &mut self,
        path: &Path,
        millis: u64,
        current_hash: u64,
    ) -> Option<(usize, usize)> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = (millis, current_hash, mtime);
        if let Some((k, counts)) = self.lane_change_counts.get(path)
            && *k == key
        {
            return *counts;
        }
        let counts = (|| {
            let snap = crate::history::snapshot_file_in(&self.history_root, path, millis)?;
            let decode = |bytes: Vec<u8>| {
                let enc = encoding_rs::Encoding::for_bom(&bytes)
                    .map(|(e, _)| e)
                    .unwrap_or(encoding_rs::UTF_8);
                enc.decode(&bytes).0.into_owned()
            };
            let before = decode(std::fs::read(snap).ok()?);
            let after = decode(std::fs::read(path).ok()?);
            let diff = similar::TextDiff::from_lines(&before, &after);
            let (mut added, mut removed) = (0, 0);
            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Delete => removed += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            Some((added, removed))
        })();
        self.lane_change_counts
            .insert(path.to_path_buf(), (key, counts));
        counts
    }

    /// Cmd+K V (#345): make the AGENT LANE section visible and open, on the
    /// Explorer, or say there is nothing in it yet.
    pub(super) fn show_agent_lane_section(&mut self) {
        if self.agent_ledger.agents().is_empty() {
            self.status = String::from("Agent Lane: no agent has changed a file yet");
            return;
        }
        if !self.explorer_views.is_visible(ExplorerView::AgentLane) {
            self.toggle_explorer_view(ExplorerView::AgentLane);
        }
        self.agent_lane_panel.collapsed = false;
        self.show_tree = true;
        self.set_sidebar_view(SidebarView::Explorer);
        self.focus_pane(Pane::Tree);
    }

    /// Lay out and paint the Explorer's visible stacked sub-views top-to-bottom
    /// inside `area` (the region above the SYSTEM footer). Folders (the file
    /// tree) is the flexible region that absorbs leftover rows; every other
    /// visible section takes its own collapsible height, clipped so the tree
    /// always keeps a few rows. Sections hidden via the ⋯ menu are skipped, and
    /// their hit-test rects are zeroed so a stale click can't reach them.
    pub(super) fn render_explorer_sections(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.open_editors.last_area = Rect::default();
        self.open_editors.save_all_btn = Rect::default();
        self.outline.last_area = Rect::default();
        self.timeline.last_area = Rect::default();
        self.dependencies.last_area = Rect::default();
        self.agent_lane_panel.last_area = Rect::default();
        let lane_rows = self.agent_lane_panel_rows();
        self.agent_lane_panel.set_rows(lane_rows);

        let folders_visible = self.explorer_views.is_visible(ExplorerView::Folders);
        if !folders_visible {
            // The tree isn't painted, so zero its geometry too — otherwise a
            // click on a since-hidden row would still resolve against it.
            self.tree.last_inner = Rect::default();
            self.tree.last_area = Rect::default();
        }
        if area.height == 0 || area.width == 0 {
            if folders_visible {
                frame.render_widget(&mut self.tree, area);
            }
            return;
        }

        // Requested rows per visible section, in stacking order. Folders is a
        // placeholder (0) here; it claims the leftover after the others.
        let mut heights: Vec<(ExplorerView, u16)> = Vec::new();
        for &v in &ExplorerView::ALL {
            if !self.explorer_views.is_visible(v) || !self.explorer_view_available(v) {
                continue;
            }
            // Offered in the ⋯ menu always, but drawn only once an agent has
            // changed something: an empty lane would cost rows for nothing.
            if v == ExplorerView::AgentLane && self.agent_lane_panel.rows().is_empty() {
                continue;
            }
            let h = match v {
                ExplorerView::Folders => 0,
                ExplorerView::OpenEditors => self.open_editors.desired_height(area.height),
                ExplorerView::Outline => self.outline.desired_height(area.height),
                ExplorerView::Timeline => self.timeline.desired_height(area.height),
                ExplorerView::Dependencies => self.dependencies.desired_height(area.height),
                ExplorerView::AgentLane => self.agent_lane_panel.desired_height(area.height),
            };
            heights.push((v, h));
        }
        if heights.is_empty() {
            return;
        }

        // Reserve a floor for the tree, then hand the fixed (non-Folders)
        // sections rows in order, clipping each to what's left.
        let total = area.height;
        let tree_floor = if folders_visible { 3u16.min(total) } else { 0 };
        let mut budget = total.saturating_sub(tree_floor);
        let mut fixed_sum = 0u16;
        for (v, h) in heights.iter_mut() {
            if *v == ExplorerView::Folders {
                continue;
            }
            let assigned = (*h).min(budget);
            *h = assigned;
            budget -= assigned;
            fixed_sum += assigned;
        }
        if folders_visible {
            if let Some(slot) = heights
                .iter_mut()
                .find(|(v, _)| *v == ExplorerView::Folders)
            {
                slot.1 = total.saturating_sub(fixed_sum);
            }
        } else {
            // No tree to absorb slack: grow the first section to fill the gap.
            let used: u16 = heights.iter().map(|(_, h)| *h).sum();
            if used < total
                && let Some(first) = heights.first_mut()
            {
                first.1 += total - used;
            }
        }

        let mut y = area.y;
        for (v, h) in heights {
            if h == 0 {
                continue;
            }
            let rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: h,
            };
            y = y.saturating_add(h);
            match v {
                ExplorerView::Folders => {
                    frame.render_widget(&mut self.tree, rect);
                    // Croft Loch: every stacked section ends on a hairline,
                    // so the tree's blank bottom padding row takes the same
                    // seam when a section follows it.
                    if let Some(t) = crate::widgets::surface::tones(self.theme)
                        && rect.bottom() < area.bottom()
                        && rect.height > 1
                    {
                        crate::widgets::surface::paint_seam(
                            frame.buffer_mut(),
                            Rect {
                                y: rect.bottom() - 1,
                                height: 1,
                                ..rect
                            },
                            crate::widgets::surface::Edge::Bottom,
                            t.hairline(),
                        );
                    }
                }
                ExplorerView::OpenEditors => frame.render_widget(&mut self.open_editors, rect),
                ExplorerView::Outline => frame.render_widget(&mut self.outline, rect),
                ExplorerView::Timeline => frame.render_widget(&mut self.timeline, rect),
                ExplorerView::Dependencies => frame.render_widget(&mut self.dependencies, rect),
                ExplorerView::AgentLane => frame.render_widget(&mut self.agent_lane_panel, rect),
            }
        }
    }

    pub(super) fn drain_fs_events(&mut self) -> bool {
        let init_changed = self.try_install_pending_init();
        let mut drain = self.fs_watch.drain(&mut self.tree, &self.editor);
        for w in &mut self.fs_watch_extra {
            let d = w.drain(&mut self.tree, &self.editor);
            drain.got_any |= d.got_any;
            drain.touched_open_file |= d.touched_open_file;
            drain.dirs_changed |= d.dirs_changed;
            drain.finder_relevant |= d.finder_relevant;
            // A secondary root's writes are an agent's writes too: dropping
            // them here would silently give a multi-root workspace an empty
            // review queue for every folder but the first (#345).
            drain.changed_files.extend(d.changed_files);
            drain.created_files.extend(d.created_files);
            drain.moved_in.extend(d.moved_in);
            drain.vanished.extend(d.vanished);
            drain.rescan_dropped_events |= d.rescan_dropped_events;
        }
        self.forward_disk_changes_to_lsp(&drain);
        if drain.dirs_changed {
            self.refresh_git_status_debounced();
            if drain.finder_relevant {
                self.kick_file_finder_index_rebuild();
            }
        }
        // `touched_open_file` only tracks the ACTIVE tab; a background tab's
        // file change still arrives as a watcher event (got_any), so sweep
        // every tab whenever any event lands. The sweep is stamp-based, so
        // a benign Metadata event that didn't move mtime+len is a no-op and
        // won't clobber selection or reload spuriously. Read-only Access
        // events never get here: `FsWatch::drain` drops them (#898).
        if drain.got_any {
            self.reload_open_file_after_external_change();
            // An open diff view follows the file the same way a buffer does
            // (#471): an agent rewriting the working file, or a save from a
            // sibling tab, re-diffs it in place.
            self.refresh_open_diff_views(false, &[], &drain.changed_files);
        }
        // Attribute the writes to whichever agents were working when they
        // landed (#345). Done after the reload so an agent's write to the
        // ACTIVE tab is hashed from the content the user is about to see.
        if !drain.changed_files.is_empty()
            && self.attribute_writes_to_working_agents(&drain.changed_files)
        {
            // Only when the ledger actually moved: the Explorer's dots must
            // match it, and a decoration that lags is worse than none.
            self.sync_agent_lane_decorations();
        }
        // A rescan means the watcher dropped events, so the queue is a
        // LOWER BOUND from here until it is emptied (#345). Recorded in the
        // ledger rather than only announced: a status line the next
        // keystroke clears would leave a partial queue looking complete.
        // Unconditional on whether an agent is working right now — the
        // dropped writes may have landed a moment ago, while the pane's
        // sampled status has since flipped to idle.
        if drain.rescan_dropped_events {
            self.agent_ledger.note_dropped_events();
            if !self.working_agents().is_empty() {
                self.status = String::from(
                    "File watcher overflowed: some agent changes may be missing from the review queue",
                );
            }
        }
        let polled = self.poll_filesystem_changes();
        drain.got_any || init_changed || polled
    }

    /// Tell the language servers about files that changed on disk (#908):
    /// a server like ty watches nothing itself, so a `git checkout` or an
    /// agent's edit to a file no tab holds stayed stale in it.
    pub(super) fn forward_disk_changes_to_lsp(&mut self, drain: &fs_watch::FsDrain) {
        if self.lsp.is_none() {
            return;
        }
        let known = self.file_finder_index.clone();
        let known = known.as_deref().map_or(&[][..], Vec::as_slice);
        let changes = fs_watch::watched_file_changes(drain, known);
        if let Some(lsp) = self.lsp.as_mut().filter(|_| !changes.is_empty()) {
            lsp.did_change_watched_files(changes);
        }
    }

    pub(super) fn handle_tree_key(&mut self, key: KeyEvent) {
        // Delete key (or Cmd+Backspace) trashes every selected node.
        if is_delete_node_key(key) {
            self.delete_selection();
            return;
        }
        if is_editor_select_all_key(key) {
            self.tree.select_all_visible();
            return;
        }
        if is_editor_copy_key(key) {
            self.copy_selection_to_explorer_clipboard(ExplorerClipMode::Copy);
            return;
        }
        if is_editor_cut_key(key) {
            self.copy_selection_to_explorer_clipboard(ExplorerClipMode::Cut);
            return;
        }
        if is_clipboard_paste_key(key) {
            self.paste_explorer_clipboard();
            return;
        }
        if is_compare_key(key) {
            self.toggle_compare_on_selected_file();
            return;
        }
        if key.code == KeyCode::Esc {
            if !self.tree.marked.is_empty() {
                self.tree.clear_marks();
            }
            return;
        }
        let extending = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up => {
                if extending {
                    self.tree.move_up_extend();
                } else {
                    self.tree.move_up();
                }
            }
            KeyCode::Down => {
                if extending {
                    self.tree.move_down_extend();
                } else {
                    self.tree.move_down();
                }
            }
            KeyCode::PageUp => {
                if extending {
                    self.tree.page_up_extend(10);
                } else {
                    self.tree.page_up(10);
                }
            }
            KeyCode::PageDown => {
                if extending {
                    self.tree.page_down_extend(10);
                } else {
                    self.tree.page_down(10);
                }
            }
            KeyCode::Home => {
                if extending {
                    self.tree.home_extend();
                } else {
                    self.tree.home();
                }
            }
            KeyCode::End => {
                if extending {
                    self.tree.end_extend();
                } else {
                    self.tree.end();
                }
            }
            KeyCode::Enter => {
                if let Some(path) = self.tree.activate() {
                    // Ctrl+Enter is the primary "open in new tab" gesture — it
                    // reaches the app on every terminal. Cmd+Enter is also
                    // accepted but iTerm2 binds it to Toggle Fullscreen by
                    // default and never forwards it to the app unless the user
                    // has remapped that shortcut in iTerm Preferences →
                    // Profiles → Keys.
                    let in_new_tab = key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META,
                    );
                    let result = if in_new_tab {
                        self.editor.open_pinned(&path)
                    } else {
                        self.editor.open_preview(&path)
                    };
                    match result {
                        Ok(()) => {
                            self.sync_open_file_poll_mtime();
                            self.status = self.editor.status.clone();
                            // Hand the keyboard to the editor, as VS Code does
                            // on Enter. Without this every navigation key the
                            // new tab defines is dead on arrival - arrows walk
                            // the file list while a PDF sits frozen on page 1
                            // and a spreadsheet never scrolls. Right-arrow
                            // (below) is the gesture that previews *without*
                            // leaving the tree.
                            self.focus_pane(Pane::Editor);
                        }
                        Err(e) => {
                            self.status = format!("Error: {e}");
                        }
                    }
                }
            }
            KeyCode::Right => {
                let selected_is_dir = self
                    .tree
                    .nodes
                    .get(self.tree.selected)
                    .is_some_and(|node| node.is_dir);
                if selected_is_dir {
                    self.tree.expand_selected();
                } else if let Some(path) = self.tree.activate() {
                    let result = self.editor.open_preview(&path);
                    match result {
                        Ok(()) => {
                            self.sync_open_file_poll_mtime();
                            self.status = self.editor.status.clone();
                        }
                        Err(e) => {
                            self.status = format!("Error: {e}");
                        }
                    }
                }
            }
            KeyCode::Left => {
                self.tree.collapse_selected();
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.apply_tree_typeahead(c, std::time::Instant::now());
            }
            _ => {}
        }
    }
}

impl App {
    /// Move every dropped path into the directory the explorer is
    /// currently pointing at. Files outside the workspace are *moved* in
    /// (matching the user's Finder-drop expectation), not copied — they
    /// disappear from the source location and re-appear in the explorer.
    pub(super) fn import_paths_into_explorer(&mut self, paths: &[PathBuf]) {
        let dest_dir = self.paste_target_dir();
        let total = paths.len();
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        affected.insert(dest_dir.clone());
        for src in paths {
            if let Some(parent) = src.parent() {
                affected.insert(parent.to_path_buf());
            }
            match crate::widgets::file_tree::move_into(&dest_dir, src) {
                Ok(p) => placed.push(p),
                Err(e) => errors.push(format!("{}: {e}", src.display())),
            }
        }
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.tree.marked.clear();
        for p in &placed {
            self.tree.marked.insert(p.clone());
        }
        if let Some(first) = placed.first()
            && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
        {
            self.tree.selected = idx;
            self.tree.anchor = idx;
        }
        self.status = if !errors.is_empty() {
            format!(
                "Imported {}/{}; failed: {}",
                placed.len(),
                total,
                errors.join("; ")
            )
        } else if total == 1 {
            format!(
                "Imported {} into {}",
                paths[0].display(),
                dest_dir.display()
            )
        } else {
            format!(
                "Imported {total} items into {}",
                self.status_path(&dest_dir)
            )
        };
    }

    pub fn consume_zoxide_jump_image_clear(&mut self) -> bool {
        self.overlays.zoxide_jump_clear.consume()
    }

    /// Open the zoxide jump popup and seed it with the full frecency list
    /// (`zoxide query -l`), so it shows the user's top directories before
    /// they type anything — like pressing `j<Tab>` in the shell.
    pub(super) fn open_zoxide_jump(&mut self) {
        if self.zoxide_jump.is_some() {
            return;
        }
        let mut jump = crate::widgets::zoxide_jump::ZoxideJump::new();
        jump.install_state = crate::zoxide::install_state();
        jump.install_note = crate::zoxide::install_outcome();
        jump.termux = crate::iterm2_inline::detect_termux();
        jump.rc_hint = crate::zoxide::shell_rc_hint();
        jump.set_results(crate::zoxide::query(""));
        // Snapshot the full frecency list so the typo-tolerant fuzzy
        // fallback can rank against it without re-spawning zoxide on every
        // missed keystroke.
        jump.set_all_dirs(jump.results.clone());
        if jump.available {
            jump.hook_offer = crate::zoxide::shell_hook_missing();
        }
        let status = if let Some(rc) = jump
            .hook_offer
            .as_deref()
            .filter(|_| jump.hook_offer_shown())
        {
            let offer = crate::widgets::zoxide_jump::hook_offer_message(rc);
            format!("Jump: {}", offer.lines().next().unwrap_or_default())
        } else if jump.available {
            format!(
                "Jump: {} directories — type to filter, Esc to close",
                jump.results.len()
            )
        } else {
            // The popup body's first line (single source of truth).
            zoxide_jump_status(&jump)
        };
        self.zoxide_jump = Some(jump);
        self.overlays.zoxide_jump_clear.request();
        self.status = status;
    }

    pub(super) fn close_zoxide_jump(&mut self) {
        self.zoxide_add_folder = false;
        if self.zoxide_jump.take().is_some() {
            self.overlays.zoxide_jump_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Re-run `zoxide query` for the popup's current text and feed the
    /// ranked matches back to the widget. The one subprocess per keystroke
    /// is cheap (zoxide reads a small frecency DB; single-digit ms) and
    /// fires only on input, never on the render hot path.
    ///
    /// Two layers: the strict `zoxide query` is authoritative and handles
    /// the common case. Only when it returns zero matches for a non-empty
    /// needle does the typo-tolerant `fuzzy_rank` fallback run, ranking the
    /// cached frecency DB by edit distance so a transposition like `spilt`
    /// still finds `pr-split` (zoxide's own subsequence matcher cannot).
    /// The fallback is purely in-memory, so it never spawns zoxide twice.
    pub(super) fn refresh_zoxide_results(&mut self) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        let needle = jump.query.clone();
        // The background install may have advanced (Running -> Done/Failed)
        // since the popup opened; keep the unavailable message current.
        jump.install_state = crate::zoxide::install_state();
        jump.install_note = crate::zoxide::install_outcome();
        match crate::zoxide::query(&needle) {
            Some(paths) if paths.is_empty() && !needle.trim().is_empty() => {
                let ranked = crate::zoxide::fuzzy_rank(&needle, &jump.all_dirs);
                if ranked.is_empty() {
                    jump.set_results(Some(Vec::new()));
                } else {
                    jump.set_approximate_results(ranked);
                }
            }
            other => jump.set_results(other),
        }
    }

    pub(super) fn handle_zoxide_jump_key(&mut self, key: KeyEvent) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.close_zoxide_jump();
            }
            (KeyCode::Enter, _) => self.apply_zoxide_jump_selection(),
            (KeyCode::Up, _) => jump.select_prev(),
            (KeyCode::Down, _) => jump.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    jump.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    jump.select_next();
                }
            }
            (KeyCode::Left, _) => jump.move_cursor_left(),
            (KeyCode::Right, _) => jump.move_cursor_right(),
            (KeyCode::Home, _) => jump.move_cursor_home(),
            (KeyCode::End, _) => jump.move_cursor_end(),
            (KeyCode::Backspace, _) => {
                jump.pop_char();
                self.refresh_zoxide_results();
            }
            (KeyCode::Delete, _) => {
                jump.delete_char();
                self.refresh_zoxide_results();
            }
            // The popup is asking "Install it? [y/N]" (zoxide is missing) or
            // "Add its `j` hook there? [y/N]" (installed, but nothing feeds it).
            (KeyCode::Char('y' | 'Y'), m)
                if jump.asks_yes_no()
                    && !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if jump.available {
                    self.add_zoxide_hook();
                } else {
                    self.start_zoxide_install();
                }
            }
            (KeyCode::Char('n' | 'N'), m)
                if jump.asks_yes_no()
                    && !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                self.close_zoxide_jump();
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                jump.push_char(c);
                self.refresh_zoxide_results();
            }
            _ => {}
        }
    }

    /// The user's `y` to the popup's install offer (#926): the only way croft
    /// ever runs the zoxide installer. A second `y` while it runs is a no-op.
    pub(super) fn start_zoxide_install(&mut self) {
        use crate::zoxide::InstallState;
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        if jump.install_state == InstallState::Running {
            return;
        }
        crate::zoxide::install_in_background();
        jump.install_state = InstallState::Running;
        self.status = zoxide_jump_status(jump);
    }

    /// The user's `y` to the popup's hook offer (#926): zoxide is installed
    /// but no `zoxide init` line feeds its database, so the managed block
    /// goes into the rc the offer named. Launching croft no longer does it.
    pub(super) fn add_zoxide_hook(&mut self) {
        let Some(rc) = self.zoxide_jump.as_mut().and_then(|j| j.hook_offer.take()) else {
            return;
        };
        self.status = match crate::zoxide::add_shell_hook() {
            Some(rc) => {
                format!("Jump: added zoxide's `j` hook to {rc}; new terminals feed this list")
            }
            None => format!("Jump: zoxide's hook was not added to {rc} (see croft's log)"),
        };
    }

    /// Once the install the user asked for ends, bring the open popup up to
    /// date without a keystroke: the directory list when it worked, the
    /// reason when it did not, and what happened in the status bar. True
    /// when something changed.
    pub(super) fn poll_zoxide_install(&mut self) -> bool {
        use crate::zoxide::InstallState;
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return false;
        };
        let state = crate::zoxide::install_state();
        if jump.install_state != InstallState::Running
            || !matches!(state, InstallState::Done | InstallState::Failed)
        {
            return false;
        }
        if state == InstallState::Done {
            jump.set_all_dirs(crate::zoxide::query("").unwrap_or_default());
        }
        self.refresh_zoxide_results();
        self.status = match (state, crate::zoxide::install_outcome()) {
            (InstallState::Done, Some(outcome)) => format!("Jump: {outcome}"),
            _ => self
                .zoxide_jump
                .as_ref()
                .map(zoxide_jump_status)
                .unwrap_or_default(),
        };
        true
    }

    /// Jump to (or add) the highlighted directory and dismiss the popup:
    /// Enter's action, shared with a mouse click on a result row.
    pub(super) fn apply_zoxide_jump_selection(&mut self) {
        let Some(jump) = self.zoxide_jump.as_ref() else {
            return;
        };
        let path = jump.selected_path().map(|p| p.to_path_buf());
        if let Some(path) = path {
            let add = self.zoxide_add_folder;
            self.close_zoxide_jump();
            // A zoxide row is a bare line from its database, so it can name a
            // directory in a spelling nothing else in croft uses (a symlinked
            // `/var/…` for `/private/var/…`). Normalise it here, where the
            // untrusted path enters, so both branches agree: the folder store
            // keys on `roots.primary().display()`, and two spellings of one
            // directory would file and restore under different keys.
            // `add_workspace_folder` canonicalises internally; the re-root
            // path must not, since its callers rely on the root being exactly
            // what they passed.
            let path = path.canonicalize().unwrap_or(path);
            if add {
                self.add_workspace_folder(path);
            } else {
                self.change_workspace_root(path);
            }
        }
    }

    pub(super) fn handle_zoxide_jump_mouse(&mut self, m: MouseEvent) {
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        let inside = rect_contains(jump.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    jump.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    jump.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_zoxide_jump();
            }
            // Click a row to jump to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = jump.row_index_at(m.row) {
                    jump.selected = idx;
                    self.apply_zoxide_jump_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_zoxide_jump(&mut self, frame: &mut ratatui::Frame) {
        let sidebar = self.last_sidebar_area;
        let full = frame.area();
        let theme = self.theme;
        let Some(jump) = self.zoxide_jump.as_mut() else {
            return;
        };
        // Anchor the popup over the Explorer column (it is an Explorer
        // action), dropping from the sidebar's top-left. The sidebar is
        // narrow, so widen the box rightward over the editor for readable
        // paths, capped to the frame. When the sidebar is collapsed there
        // is nothing to anchor to, so fall back to a centered box.
        let rect = match sidebar {
            Some(side) if side.width >= 2 && side.height >= 4 => {
                let width = side
                    .width
                    .max(ZOXIDE_JUMP_MIN_WIDTH)
                    .min(full.width.saturating_sub(side.x));
                Rect {
                    x: side.x,
                    y: side.y,
                    width,
                    height: side.height,
                }
            }
            _ => fallback_popup_rect(full, 100),
        };
        crate::widgets::zoxide_jump::render_zoxide_jump(jump, rect, frame.buffer_mut(), theme);
    }

    /// Add `path` as another workspace root (multi-root, #147): the model
    /// grows, the Explorer gains the section, every root gets a watcher
    /// share, the Cmd+P index and workspace search span the new set, and
    /// the LSP layer widens its project-root bound (servers for the new
    /// root spawn per (language, project root) exactly like a fresh
    /// workspace's — croft's per-project server model needs no
    /// didChangeWorkspaceFolders, since no running server's folder set
    /// changes). Session/collab identity deliberately stays keyed on the
    /// PRIMARY root: adding a folder never changes what the window IS.
    pub fn add_workspace_folder(&mut self, path: PathBuf) {
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            self.status = format!("Not a folder: {}", path.display());
            return;
        }
        if !self.roots.add(path.clone()) {
            self.status = String::from("Folder is already in the workspace");
            return;
        }
        self.tree.add_root(path.clone());
        crate::file_encoding::set_workspace_default(&path, self.files_encoding);
        self.reload_project_snippets();
        self.git_extra
            .push((path.clone(), GitWorker::spawn(path.clone())));
        self.rebind_workspace_watchers();
        self.sync_workspace_fanout();
        self.refresh_workspace_title();
        self.save_workspace_folders();
        self.install_file_associations(self.files_associations.clone());
        self.status = format!("Added folder: {}", path.display());
    }

    /// Remove a SECONDARY workspace root (#147). The primary is refused —
    /// it is the launch identity; changing it is Make Root's job. Open
    /// tabs under the removed root stay open (they become plain
    /// out-of-workspace files, VS Code behavior).
    pub fn remove_workspace_folder(&mut self, path: PathBuf) {
        if !self.roots.remove(&path) {
            self.status = String::from(
                "The primary workspace folder cannot be removed (use Make Root to change it)",
            );
            return;
        }
        self.tree.remove_root(&path);
        self.install_file_associations(self.files_associations.clone());
        crate::file_encoding::set_workspace_default(&path, None);
        self.snippets = load_all_snippets(self.roots.iter());
        // The removed folder's review queue goes with it (#345). Its rows
        // leave the tree, so no stale DOT renders — but the status chip
        // counts the same set, and a count that outlives the folder it
        // describes sends the user looking for rows that are not there.
        for gone in self
            .agent_ledger
            .unreviewed_paths()
            .into_iter()
            .filter(|p| p.starts_with(&path))
        {
            self.agent_ledger.forget_path(&gone);
        }
        self.sync_agent_lane_decorations();
        self.git_extra.retain(|(r, _)| r != &path);
        self.git_head_oids.remove(&path);
        if self.active_scm_root == path {
            // The panel was following the removed folder's repo: seed the
            // primary's immediately — assigning the field alone left the
            // removed repository on screen (#150 review).
            let primary = self.roots.primary().to_path_buf();
            self.seed_active_scm(primary);
        }
        self.rebind_workspace_watchers();
        self.sync_workspace_fanout();
        self.refresh_workspace_title();
        self.save_workspace_folders();
        self.status = format!("Removed folder: {}", path.display());
    }

    /// (Re)spawn one watcher per workspace root, splitting the per-OS
    /// watch caps across them (#147): the caps bound kernel resources per
    /// PROCESS, so N roots share one budget rather than multiplying it.
    pub(super) fn rebind_workspace_watchers(&mut self) {
        let shares = self.roots.iter().count();
        let primary = self.workspace_root().to_path_buf();
        self.fs_watch.rebind_sharing(&primary, &self.tree, shares);
        self.fs_watch_extra = self
            .roots
            .iter()
            .skip(1)
            .map(|r| FsWatch::spawn_event_only(r, shares))
            .collect();
    }

    /// Push the current root set into the per-root fan-outs that are not
    /// watcher-shaped: the Cmd+P index, the search worker + panel, and
    /// the LSP project-root bound.
    pub(super) fn sync_workspace_fanout(&mut self) {
        self.file_finder_index = None;
        self.file_finder = None;
        self.kick_file_finder_index_rebuild();
        let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        let _ = self
            .search_query_tx
            .send(SearchRequest::SetRoots(roots.clone()));
        self.search.roots = roots.clone();
        if let Some(lsp) = &self.lsp {
            lsp.set_extra_roots(roots.into_iter().skip(1).collect());
        }
    }

    /// Re-emit the OSC window title for the current folder set: the
    /// primary root's name, with VS Code's " (Workspace)" marker while
    /// more than one folder is open (#147).
    pub(super) fn refresh_workspace_title(&mut self) {
        use std::io::Write;
        let mut title = build_title(self.workspace_root());
        if self.roots.is_multi() {
            title.push_str(" (Workspace)");
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(&set_title_seq(&title));
        let _ = out.flush();
    }

    /// Persist the current secondary-folder list under the primary
    /// root's key (#153) — the terminal-session model: an empty list
    /// prunes the key.
    pub(super) fn save_workspace_folders(&mut self) {
        let primary = self.roots.primary().display().to_string();
        let folders: Vec<PathBuf> = self.roots.iter().skip(1).map(Path::to_path_buf).collect();
        if let Err(e) =
            crate::workspace::save_folders_for_root(&self.workspace_folders_path, &primary, folders)
        {
            // A corrupt store is refused, never replaced (#156 review):
            // the arrangement still works this session, it just won't
            // persist until the store is repaired or removed.
            self.status = format!("Workspace folders not saved: {e}");
        }
    }

    /// Restore the saved folder set for the current primary root (#153):
    /// each surviving folder re-joins through the normal add fan-out; a
    /// folder that no longer exists on disk is dropped from the record
    /// rather than restored.
    pub fn restore_workspace_folders(&mut self) -> usize {
        let map = crate::workspace::load_folders(&self.workspace_folders_path);
        let primary = self.roots.primary().display().to_string();
        let Some(saved) = map.get(&primary).cloned() else {
            return 0;
        };
        let before = self.roots.iter().count();
        for folder in saved {
            if folder.is_dir() {
                self.add_workspace_folder(folder);
            }
        }
        // Rewrites the record from what actually restored, dropping
        // vanished folders (and pruning the key if none survived).
        self.save_workspace_folders();
        // The caller composes the user-facing message: setting status
        // here was immediately overwritten on the re-root path (#156
        // review).
        self.roots.iter().count() - before
    }

    /// Apply a `.code-workspace` file (#163): re-root to its first folder
    /// and install its folder set exactly (the pending set suppresses the
    /// automatic store restore for this re-root).
    pub(super) fn open_workspace_file(&mut self, path: &Path) {
        match crate::workspace::parse_workspace_file(path) {
            Ok(folders) => {
                let mut it = folders.into_iter();
                let primary = it.next().expect("parse guarantees a folder");
                if !primary.is_dir() {
                    // Refuse before touching the workspace: re-rooting
                    // into a vanished folder would strand the window
                    // (#164 review).
                    self.status = format!("Workspace file: missing folder {}", primary.display());
                    return;
                }
                self.pending_workspace_folders = Some(it.collect());
                self.change_workspace_root(primary);
            }
            Err(e) => self.status = format!("Workspace file: {e}"),
        }
    }

    /// Save the folder set to the `.code-workspace` at `dest`: a new file,
    /// or the `folders` of an existing one (#1012), and say where.
    pub(super) fn save_workspace_file(&mut self, dest: &Path) {
        let new_folder = dest.parent().filter(|p| !p.exists()).map(Path::to_path_buf);
        let folders: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        match crate::workspace::write_workspace_file(dest, &folders) {
            Ok(()) => {
                self.status = format!(
                    "Saved workspace ({} folder{}): {}{}",
                    folders.len(),
                    if folders.len() == 1 { "" } else { "s" },
                    dest.display(),
                    new_folder
                        .map(|f| format!(" (created folder {})", f.display()))
                        .unwrap_or_default()
                );
            }
            Err(e) => self.status = format!("Save workspace: {e}"),
        }
    }

    /// Open the fuzzy directory picker in add-folder mode (#147): the
    /// same zoxide-backed popup as Cmd+Z, with Enter adding the chosen
    /// directory as a workspace root instead of re-rooting.
    pub(super) fn open_add_folder_picker(&mut self) {
        self.zoxide_add_folder = true;
        self.open_zoxide_jump();
        if self.zoxide_jump.is_some() {
            self.status =
                String::from("Add Folder to Workspace: pick a directory — Enter adds, Esc cancels");
        }
    }

    /// Re-root the workspace at `new_root`. Resets the file tree, updates
    /// `workspace_root`, refreshes git status and the source-control
    /// panel, and respawns the FS watcher off-thread (a recursive stat
    /// walk on a multi-GB monorepo is the dominant cost - blocking the
    /// UI thread for it is what froze the app on "Make root"). The
    /// polling fallback in `drain_fs_events` keeps the tree fresh until
    /// the watcher comes online via `try_install_pending_init`. Open
    /// editor tabs are NOT closed - a path that escapes the new root
    /// still resolves on disk and the user may want to keep it open.
    /// Compare anchor / clipboard / marks are reset because they point
    /// into the old workspace.
    pub fn change_workspace_root(&mut self, new_root: PathBuf) {
        // The review queue being left is saved under its own root first.
        self.persist_agent_ledger(true);
        let display = new_root.display().to_string();
        // The remembered task belongs to the old workspace; Rerun Last Task
        // must rediscover, not rerun the old project's command here.
        self.last_task = None;
        // Read the panel's untouched-ness BEFORE the cd seed below: the seed
        // is an input write, which clears it. Then persist the outgoing
        // workspace's pane layout under ITS key while `workspace_root` still
        // names it — every save after the swap keys on the new root, which
        // both discarded this layout on re-root and filed the old panes
        // under the new workspace's entry (#137).
        let panel_pristine = self.terminals.len() == 1 && self.terminals[0].is_pristine();
        self.save_terminal_session();
        let mut shell_synced = true;
        if let Some(active) = self.terminals.get_mut(self.active_terminal) {
            // cwd_seed_is_safe, not foreground_is_shell: a pane whose shell
            // is still starting up (rc files own the tty, no input ever
            // written, no prompt yet) must still get the seed — it queues as
            // type-ahead for the first prompt (#94).
            if active.cwd_seed_is_safe() {
                active.change_cwd(&new_root);
            } else {
                shell_synced = false;
            }
        }
        // The hot-exit backup is keyed by the workspace: this croft's goes
        // with it to the new root at the next tick, rather than staying
        // where the old workspace's next launch would restore it (#862).
        self.remove_own_hot_exit();
        // The `files_encoding` defaults of the folders being left go with
        // them (#1432). The settings merge below sets only the new roots,
        // and a default left on a parent folder would still prefix-match
        // every file of a nested new root and decode it in the old encoding.
        for root in self.roots.iter() {
            crate::file_encoding::set_workspace_default(root, None);
        }
        self.roots.replace_primary(new_root.clone());
        // The old repository's snippets go, the new one's apply (#1484).
        self.reload_project_snippets();
        // A panel the user never touched — the startup default, one unnamed
        // shell with no input ever typed and no launched program — is swapped
        // for the incoming workspace's saved layout, the same restore startup
        // runs. A panel with any user state instead survives the re-root
        // as-is: navigation must never destroy live shells.
        if panel_pristine {
            self.restore_terminal_session();
        }
        // Rebind the LSP servers to the new root. The manager captured its
        // workspace folder once, at App::new, from croft's launch directory -
        // and the Croft.app launcher hardcodes that to ~/Documents. Re-rooting
        // into a child repo left rust-analyzer running `cargo metadata` against
        // the parent, which has no Cargo.toml, so every opened .rs came back as
        // an `unlinked-file` with no hover / completion / semantic tokens.
        // Spawn a fresh manager rooted at new_root. The old one's Drop shuts
        // its servers down and waits up to 3 s for them, so it is dropped
        // off the UI thread, as the build pause does (#737). Clearing
        // lsp_last_seen makes the next sync_lsp() re-open every live editor tab
        // against the new servers; the stale diagnostics / progress from the
        // old root are dropped with it.
        //
        // Not while a source build of croft has the servers stopped (#694):
        // starting them now would put rust-analyzer back beside the compile.
        // The manager drops either way, and the end of the build starts one
        // at the new root, since the resume reads `workspace_root()`.
        if let Some(old) = self.lsp.take() {
            fs_watch::offload_drop(old);
        }
        self.lsp = match self.lsp_paused_for_build {
            true => None,
            false => match crate::lsp::LspManager::new(new_root.clone()) {
                Ok(m) => Some(m),
                Err(e) => {
                    // Not stderr: the TUI owns the screen here, and text written
                    // around ratatui stays on it (#537's class of leak).
                    let msg = format!("Language servers could not restart: {e}");
                    crate::output::push(
                        "Language Servers",
                        crate::output::OutputLevel::Error,
                        &msg,
                    );
                    self.status = msg;
                    None
                }
            },
        };
        self.lsp_last_seen.clear();
        self.lsp_diagnostics.clear();
        self.lsp_progress.clear();
        // A new manager numbers semantic replies from 0 again (see the
        // build resume, which hits the same thing).
        self.semantic_generation_seen.clear();
        // Build diagnostics describe the OLD workspace's files; a re-root
        // that kept them showed the previous project's errors forever.
        self.build_diagnostics.clear();
        self.build_diag_files_by_pane.clear();
        // Refresh the window/icon title: it was emitted once at startup from
        // the launch dir and never updated, so after a re-root it kept showing
        // the parent (e.g. "Documents") instead of the repo. OSC 0 is
        // grid-independent, so writing it straight to stdout between draws is
        // safe - the same pattern startup uses.
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(&set_title_seq(&build_title(&new_root)));
            let _ = out.flush();
        }
        self.tree.set_root(new_root.clone());
        self.tree_clipboard = None;
        self.compare_anchor = None;
        // The agent review queue belongs to the workspace that was left
        // (#345). `set_root` has just cleared the Explorer's copy; this
        // clears the ledger it is drawn from, so the two cannot come back
        // apart the moment an agent writes under the new root. The new
        // root's own saved queue comes back in its place.
        self.agent_ledger = self.load_agent_ledger(&new_root);
        self.fs_watch.rebind(&new_root, &self.tree);
        // A re-root collapses the workspace to one folder: the secondary
        // watchers go with their roots (#147).
        self.fs_watch_extra.clear();
        // A re-root collapses the git side too (#149): the secondary
        // workers drop with their roots, the active repo IS the new
        // primary, and the per-repo HEAD markers restart.
        self.git_extra.clear();
        self.active_scm_root = new_root.clone();
        self.git_head_oids.clear();
        self.git.bypass_debounce();
        // Rebind the worker's root before any query, otherwise the next
        // Status / Changes request runs against the stale root captured
        // at App::new and the Source Control panel keeps reporting the
        // OLD repo's state (or "no repo") even after a Make Root that
        // landed inside a real git tree.
        self.git.set_root(new_root.clone());
        self.refresh_git_status_debounced();
        // Drop the old repo's graph before the refresh (inside
        // refresh_source_control) refetches for the new root; a reply still
        // in flight for the old root is dropped by the drain's root tag.
        self.commit_graph.clear();
        self.refresh_source_control();
        // Rebind the test runner too, else `cargo test -- --list` keeps running
        // in the launch dir captured at spawn (e.g. ~/Documents, no Cargo.toml)
        // and the Testing view stays empty after a Make Root into a child repo.
        // Then drop the old project's tree: its cases are unrelated, they keep
        // the beaker badge counting dead failures, and a non-empty panel blocks
        // open_testing_view's discover-on-empty, so the new project would never
        // be discovered. Results still streaming from the old root are dropped
        // by the drain's epoch tag (same idea as the commit graph's root tag).
        // With the Testing view in front, re-discover right away; otherwise the
        // now-empty panel discovers on its next open.
        self.active_test_root = new_root.clone();
        self.test_worker.set_root(new_root.clone());
        self.testing.reset();
        // Drop any in-flight `cargo test --no-run`: the drain would refuse
        // its root-mismatched binary anyway, but the occupied slot kept
        // refusing NEW debug builds until the stale compile finished.
        self.disarm_failure_breakpoint();
        self.pending_test_debug = None;
        if self.sidebar_view == SidebarView::Testing {
            self.discover_tests();
        }
        // Rebind the search worker and the panel's display root to the new
        // Explorer root so the next search targets this directory and its
        // subdirectories, not the root captured when the worker was spawned.
        // Drop the old root's hits so nothing stale lingers in the panel.
        let _ = self
            .search_query_tx
            .send(SearchRequest::SetRoot(new_root.clone()));
        self.search.root = new_root.clone();
        self.search.roots = vec![new_root.clone()];
        self.search.hits.clear();
        self.search.selected = 0;
        self.search.scroll = 0;
        // Drop any in-flight walker for the OLD root so its result is
        // discarded when it tries to send. Then kick a fresh walk
        // against the new root. file_finder_index keeps the old entries
        // for now so a Cmd+P press during the rebuild window shows
        // stale-but-immediate results instead of a UI freeze.
        self.file_finder_index_rx = None;
        self.file_finder_index_dirty = false;
        self.kick_file_finder_index_rebuild();
        // Rebind the resident navigator: the activation record, relay
        // socket, and single-host lock are all keyed by workspace root
        // (captured at App::new), so without this a `croft pair` run in the
        // NEW root writes a record this window never reads and the
        // navigator can never seat again after a re-root. Any seat from the
        // old root is torn down with its comment boxes; if this window had
        // self-appointed the old root's collab owner, that session goes too
        // (a fresh activation self-appoints against the new root).
        if let Some(host) = self.pair_host.take() {
            let sites = host.caret_sites();
            self.collab_carets.retain(|s, _| !sites.contains(s));
            self.status = String::from("Navigator unseated (workspace changed)");
        }
        self.navigator_notes.clear();
        self.pair_commentary_buf.clear();
        self.pair_turn_origin = None;
        self.clear_problem_fix();
        self.editor.comment_focus = None;
        if self.pair_host_lock.take().is_some() {
            self.collab = None;
            self.collab_config = None;
        }
        // Old-root merge bases are keyed by root-relative file, like the
        // carets below: never let them merge into a same-named file here.
        self.collab_offline_base.clear();
        self.pair_record_path = crate::session::pair_record_path(&new_root);
        self.pair_socket = crate::session::collab_socket_path(&new_root);
        self.pair_host_lock_path = crate::session::pair_host_lock_path(&new_root);
        self.navigator_down = false; // the death latch belonged to the old root
        self.last_pair_check = None; // read the new record this tick, not in 1s
        // Old-workspace carets are keyed by root-relative file, so a
        // same-named file under the new root would wear a departed peer's
        // caret. The live session repopulates its own on the next poll.
        self.collab_carets.clear();
        // The new primary's saved folder set comes back with it (#153),
        // the same way its terminal layout does: re-rooting into a
        // workspace restores that workspace's arrangement.
        let restored = match self.pending_workspace_folders.take() {
            Some(folders) => {
                let before = self.roots.iter().count();
                for folder in folders {
                    self.add_workspace_folder(folder);
                }
                self.roots.iter().count() - before
            }
            None => self.restore_workspace_folders(),
        };
        let folders_note = if restored > 0 {
            format!(
                " (+{restored} workspace folder{})",
                if restored == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        // The workspace settings layers follow the primary root (#251): the
        // old repo's .croft/config.json must stop applying and the new one
        // start, without a relaunch. Runs after the root swap above so the
        // merge reads the new primary. The status write below is deliberate:
        // remerge_settings sets its own status, and the re-root message is
        // the one that should win here.
        self.remerge_settings();
        self.status = if shell_synced {
            format!("Workspace root: {display}{folders_note}")
        } else {
            format!("Workspace root: {display}{folders_note} (terminal busy; shell not moved)")
        };
    }

    pub(super) fn is_explorer_focused(&self) -> bool {
        self.focus == Pane::Tree && self.sidebar_view == SidebarView::Explorer
    }

    /// Dispatch the Explorer-pane keyboard shortcuts (Cmd+F / Cmd+Shift+F /
    /// Cmd+R / F2 / Cmd+/). Returns true if the key was handled. The
    /// target node is whichever row the user has highlighted; folder-only
    /// actions (Make root) are no-ops on file selections.
    pub(super) fn handle_explorer_shortcut(&mut self, key: KeyEvent) -> bool {
        if is_tree_zoxide_jump_key(key) {
            self.open_zoxide_jump();
            return true;
        }
        if is_tree_new_file_key(key) {
            let target = self.explorer_create_target_dir();
            self.open_create_prompt(CreateKind::File, target);
            return true;
        }
        if is_tree_new_folder_key(key) {
            let target = self.explorer_create_target_dir();
            self.open_create_prompt(CreateKind::Folder, target);
            return true;
        }
        if is_tree_rename_key(key) {
            if let Some(path) = self.explorer_selected_path() {
                self.open_rename_prompt(path);
            }
            return true;
        }
        // Reveal in Finder is a local-macOS-only host bridge: on a remote SSH
        // session croft runs on a headless host with no Finder, so the chord
        // falls through there exactly as the menu entry is withheld.
        if !self.is_remote && cfg!(target_os = "macos") && is_reveal_in_finder_key(key) {
            if let Some(path) = self.explorer_selected_path() {
                self.reveal_in_finder(path);
            }
            return true;
        }
        if is_tree_make_root_key(key) {
            if let Some(path) = self.explorer_selected_folder() {
                self.change_workspace_root(path);
            }
            return true;
        }
        if is_tree_make_parent_root_key(key) {
            if let Some(path) = self.explorer_parent_for_make_root() {
                self.change_workspace_root(path);
            }
            return true;
        }
        false
    }

    /// Resolve the parent folder of the highlighted node for the
    /// `Cmd+Shift+/` "Make root at parent" gesture. Works on files
    /// (parent = containing directory), folders (parent = its parent),
    /// and the tree root itself (parent = one filesystem level up).
    /// Returns `None` when the selected path has no parent (filesystem
    /// root) or no node is currently selected.
    pub(super) fn explorer_parent_for_make_root(&self) -> Option<PathBuf> {
        let n = self.tree.nodes.get(self.tree.selected)?;
        n.path.parent().map(|p| p.to_path_buf())
    }

    pub(super) fn explorer_selected_path(&self) -> Option<PathBuf> {
        self.tree
            .nodes
            .get(self.tree.selected)
            .map(|n| n.path.clone())
    }

    pub(super) fn explorer_selected_folder(&self) -> Option<PathBuf> {
        let n = self.tree.nodes.get(self.tree.selected)?;
        if n.is_dir && n.path != self.tree.root {
            Some(n.path.clone())
        } else {
            None
        }
    }

    pub(super) fn explorer_create_target_dir(&self) -> PathBuf {
        let node = self.tree.nodes.get(self.tree.selected);
        crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root)
    }

    /// Where the palette's File: New File… / New Folder… create (#852): the
    /// Explorer selection while the Explorer has focus, as its own chords
    /// do; otherwise beside the active file, falling back to the selection
    /// when no file is open.
    pub(super) fn palette_create_target_dir(&self) -> PathBuf {
        if !self.is_explorer_focused()
            && let Some(dir) = self.editor.path.as_deref().and_then(Path::parent)
        {
            return dir.to_path_buf();
        }
        self.explorer_create_target_dir()
    }

    /// VS Code-style type-to-jump for the Explorer. Each printable
    /// keystroke arriving while the file tree is focused appends to a
    /// short-lived prefix buffer and snaps selection to the first
    /// visible node whose name starts with that prefix. Keystrokes
    /// further apart than `TREE_TYPEAHEAD_WINDOW` reset the buffer to
    /// a single character and cycle to the next match, so re-pressing
    /// the same letter walks through siblings beginning with it.
    /// `now` is injected for deterministic tests.
    pub(super) fn apply_tree_typeahead(&mut self, c: char, now: std::time::Instant) {
        let within_window = self
            .tree_typeahead
            .as_ref()
            .is_some_and(|t| now.duration_since(t.last) < TREE_TYPEAHEAD_WINDOW);
        let prefix = if within_window {
            let mut p = self.tree_typeahead.as_ref().unwrap().prefix.clone();
            p.push(c.to_ascii_lowercase());
            p
        } else {
            c.to_ascii_lowercase().to_string()
        };
        let n = self.tree.nodes.len();
        if n > 0 {
            let start = if within_window {
                self.tree.selected
            } else {
                (self.tree.selected + 1) % n
            };
            if let Some(idx) = self.tree.find_prefix(&prefix, start) {
                self.tree.selected = idx;
                self.tree.anchor = idx;
                self.tree.marked.clear();
            }
        }
        self.tree_typeahead = Some(TreeTypeahead { prefix, last: now });
    }

    /// Trash every entry in the tree's current action set (the multi-
    /// selection if non-empty, otherwise just the focused row). Workspace
    /// root rows are filtered by `delete_paths` — the shared choke point —
    /// even if some other path in the set proceeds.
    pub(super) fn delete_selection(&mut self) {
        let paths = self.tree.action_paths();
        if paths.is_empty() {
            return;
        }
        self.delete_paths(paths);
    }

    pub(super) fn delete_paths(&mut self, paths: Vec<PathBuf>) {
        // No workspace root row is ever a delete target, whichever flow
        // assembled the set: the context menu forwards a mixed multi-
        // selection wholesale, and Select All marks the root rows too, so
        // the per-node `delete_target_for` guard alone cannot protect a
        // root riding along in the batch. Filter here — the one choke
        // point every delete flow funnels through — with the same
        // canonicalized comparison the old primary-root guard used, so a
        // root reached through a symlinked path is still refused.
        let canon_roots: Vec<PathBuf> = self
            .tree
            .root_paths()
            .filter_map(|r| r.canonicalize().ok())
            .collect();
        let is_root = |p: &PathBuf| {
            self.tree.root_paths().any(|r| r == p)
                || p.canonicalize()
                    .ok()
                    .is_some_and(|cp| canon_roots.contains(&cp))
        };
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| !is_root(p)).collect();
        let total = paths.len();
        if total == 0 {
            self.status = String::from("Workspace roots cannot be deleted");
            return;
        }
        // Confirm before trashing — Delete (or the context menu's Delete) must
        // not remove files from a single action. Enter or Y performs it; Esc
        // or N keeps them, and there is no field to type a different answer
        // into (#855). Files go to the OS Trash (recoverable), so the wording
        // reflects that.
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let title = if total == 1 {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| paths[0].display().to_string());
            format!("Move '{name}' to Trash?")
        } else {
            format!("Move {total} items to Trash?")
        };
        self.open_input_prompt(InputPrompt::confirm(
            InputPurpose::TreeDelete { paths },
            title,
            "Enter or Y to trash · Esc or N to keep",
        ));
    }

    /// Re-point the tabs of a renamed or moved path in every editor group,
    /// not just the focused one: a tab in the other split kept the old path,
    /// raised a disk conflict, and a save brought the old file back.
    pub(super) fn rename_open_path_everywhere(&mut self, old: &Path, new: &Path) {
        self.editor.rename_open_path(old, new);
        for group in self.editor_layout.inactive_groups_mut() {
            group.rename_open_path(old, new);
        }
    }

    /// Perform the confirmed trash of `paths` (the popup's Enter path).
    pub(super) fn perform_delete_paths(&mut self, paths: Vec<PathBuf>) {
        let total = paths.len();
        if total == 0 {
            return;
        }
        let mut affected_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for path in &paths {
            let parent = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.tree.root.clone());
            affected_dirs.insert(parent);
        }
        // One trash call for the whole batch — on macOS this routes through
        // Finder so the trash sound effect fires once for the operation
        // instead of once per file. The single-path path keeps the cheaper
        // NSFileManager backend.
        let result = if total == 1 {
            crate::widgets::file_tree::move_to_trash(&paths[0])
        } else {
            crate::widgets::file_tree::move_to_trash_bulk(&paths)
        };
        match result {
            Ok(()) => {
                // Every group's tabs, and files under a deleted folder too.
                // A tab with unsaved edits is kept open: closing it threw the
                // edits away while the trash only held the saved copy.
                let mut kept = 0;
                for path in &paths {
                    kept += self.editor.close_clean_tabs_under(path);
                    for group in self.editor_layout.inactive_groups_mut() {
                        kept += group.close_clean_tabs_under(path);
                    }
                }
                self.sync_open_file_poll_mtime();
                for dir in &affected_dirs {
                    if let Some(idx) = self.tree.index_of_dir(dir) {
                        self.tree.refresh_children(idx);
                    }
                }
                self.tree.clear_marks();
                self.status = if total == 1 {
                    format!("Moved {} to Trash", paths[0].display())
                } else {
                    format!("Moved {total} items to Trash")
                };
                if kept > 0 {
                    self.status.push_str(&format!(
                        "; {kept} tab{} with unsaved changes kept open",
                        if kept == 1 { "" } else { "s" }
                    ));
                }
            }
            Err(e) => {
                for dir in &affected_dirs {
                    if let Some(idx) = self.tree.index_of_dir(dir) {
                        self.tree.refresh_children(idx);
                    }
                }
                self.status = if total == 1 {
                    format!("Delete failed: {e}")
                } else {
                    format!("Bulk delete failed: {e}")
                };
            }
        }
    }

    /// Stash the current action paths into the explorer clipboard. The
    /// system text clipboard is left untouched so editor/terminal text
    /// copy gestures still work elsewhere in the app.
    pub(super) fn copy_selection_to_explorer_clipboard(&mut self, mode: ExplorerClipMode) {
        let paths = self.tree.action_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.tree_clipboard = Some(ExplorerClipboard { mode, paths });
        self.status = match (mode, n) {
            (ExplorerClipMode::Copy, 1) => String::from("Copied 1 item"),
            (ExplorerClipMode::Copy, _) => format!("Copied {n} items"),
            (ExplorerClipMode::Cut, 1) => String::from("Cut 1 item"),
            (ExplorerClipMode::Cut, _) => format!("Cut {n} items"),
        };
    }

    /// Resolve the directory that an explorer paste/drop should land in,
    /// based on the currently focused node. Mirrors `create_target_dir_for`
    /// but for keyboard paste (no right-click coordinates).
    pub(super) fn paste_target_dir(&self) -> PathBuf {
        let node = self.tree.nodes.get(self.tree.selected);
        crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root)
    }

    /// Move (Cut) or copy (Copy) every clipboard path into `dest_dir`,
    /// then refresh affected directories and select the freshly-pasted
    /// items. Cut clears the explorer clipboard on success; Copy preserves
    /// it so the user can paste again.
    pub(super) fn paste_explorer_clipboard(&mut self) {
        let dest_dir = self.paste_target_dir();
        self.paste_into(dest_dir);
    }

    /// `Ctrl/Cmd+D` from the explorer: smart toggle that mirrors the right-
    /// click "Select for Compare" / "Compare with Selected" / clear-anchor
    /// chain in a single key press.
    pub(super) fn toggle_compare_on_selected_file(&mut self) {
        let Some(node) = self.tree.nodes.get(self.tree.selected) else {
            self.status = String::from("No file selected to compare");
            return;
        };
        if node.is_dir {
            self.status = String::from("Compare needs a file, not a folder");
            return;
        }
        let path = node.path.clone();
        match self.compare_anchor.as_ref() {
            Some(anchor) if anchor == &path => {
                self.compare_anchor = None;
                self.status = String::from("Cleared compare anchor");
            }
            Some(anchor) => {
                let anchor_clone = anchor.clone();
                match self.open_diff_across_groups(&anchor_clone, &path) {
                    Ok(()) => {
                        self.focus_pane(Pane::Editor);
                        self.compare_anchor = None;
                        self.sync_open_file_poll_mtime();
                        let l = anchor_clone
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| anchor_clone.display().to_string());
                        let r = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        self.status = format!("Diff: {l} \u{2194} {r}");
                    }
                    Err(e) => {
                        self.status = format!("Diff failed: {e}");
                    }
                }
            }
            None => {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.compare_anchor = Some(path);
                self.status =
                    format!("Selected {label} for compare — Ctrl/Cmd+D again on another file");
            }
        }
    }

    pub(super) fn paste_into(&mut self, dest_dir: PathBuf) {
        let Some(clip) = self.tree_clipboard.clone() else {
            self.status = String::from("Explorer clipboard is empty");
            return;
        };
        let moved = self.move_or_copy_in_explorer(&dest_dir, &clip.paths, clip.mode);
        // A cut pasted back where it already is moved nothing, so it stays
        // on the clipboard for the paste into the folder it was meant for.
        if moved && matches!(clip.mode, ExplorerClipMode::Cut) {
            self.tree_clipboard = None;
        }
    }

    /// Explorer paste and drag-drop. A move asks the language servers for
    /// their import edits first (#610); a copy renames nothing, so it runs
    /// at once.
    pub(super) fn move_or_copy_in_explorer(
        &mut self,
        dest_dir: &Path,
        paths: &[PathBuf],
        mode: ExplorerClipMode,
    ) -> bool {
        if matches!(mode, ExplorerClipMode::Copy) {
            let _ = self.apply_paste_or_drop(dest_dir, paths, mode);
            return true;
        }
        // An item already in `dest_dir` stays as it is: moving it there is
        // no move, and a fresh name for it renamed the file to "<name> copy"
        // on disk, retargeting its tabs and the servers' imports (#1174).
        let paths: Vec<PathBuf> = paths
            .iter()
            .filter(|p| !crate::widgets::file_tree::is_directly_in(dest_dir, p))
            .cloned()
            .collect();
        if paths.is_empty() {
            self.status = String::from("Already in this folder: nothing to move");
            return false;
        }
        let renames = paths
            .iter()
            .map(|src| crate::lsp::manager::FileRenameOp {
                old: src.clone(),
                new: crate::widgets::file_tree::unique_destination_in(dest_dir, src),
                is_dir: src.is_dir(),
            })
            .collect();
        let op = FileMove::Paste {
            dest_dir: dest_dir.to_path_buf(),
            paths,
        };
        self.start_file_move(op, renames);
        true
    }

    /// Ask the servers about `renames`, then run `op` once they answer (or
    /// at once, with no language server running).
    pub(super) fn start_file_move(
        &mut self,
        op: FileMove,
        renames: Vec<crate::lsp::manager::FileRenameOp>,
    ) {
        // A second move while one is pending runs the first now: two moves
        // must not reorder, and the first has waited long enough.
        if let Some(prev) = self.pending_file_move.take() {
            self.finish_file_move(prev, Vec::new());
        }
        let Some(lsp) = self
            .lsp
            .as_mut()
            .filter(|l| l.any_server_wants_file_renames())
        else {
            if self.run_file_move(op)
                && let Some(lsp) = self.lsp.as_mut()
            {
                lsp.did_rename_files(renames);
            }
            return;
        };
        let request_id = lsp.request_will_rename_files(renames.clone());
        self.status = String::from("Updating references before the move\u{2026}");
        self.pending_file_move = Some(PendingFileMove {
            request_id,
            op,
            renames,
            deadline: std::time::Instant::now() + FILE_MOVE_DEADLINE,
            baseline: self.edit_seqs_in_every_group(),
        });
    }

    /// Land the servers' answer to a pending Explorer move, or give up on
    /// them at the deadline and move anyway. Returns true when the screen
    /// changed.
    pub fn tick_file_moves(&mut self) -> bool {
        let Some(pending) = self.pending_file_move.as_ref() else {
            return false;
        };
        let mut answer = None;
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(result) = lsp.drain_will_rename_files() {
                if result.request_id == pending.request_id {
                    answer = Some(result.edits);
                }
            }
        }
        if answer.is_none() && std::time::Instant::now() < pending.deadline {
            return false;
        }
        let pending = self.pending_file_move.take().expect("checked above");
        self.finish_file_move(pending, answer.unwrap_or_default());
        true
    }

    /// Run the move, then apply the servers' edits and tell the servers it
    /// happened. The move goes first: a move that fails (a folder into its
    /// own child, a name already taken) must not leave every importer
    /// rewritten to a path that does not exist. The edits name files at
    /// their old paths, so they are carried to where the move put them.
    pub(super) fn finish_file_move(
        &mut self,
        pending: PendingFileMove,
        edits: Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)>,
    ) {
        if !self.run_file_move(pending.op) {
            return;
        }
        let moved_status = self.status.clone();
        if !edits.is_empty() {
            let edits: Vec<_> = edits
                .into_iter()
                .map(|(path, e)| (moved_path(&path, &pending.renames), e))
                .collect();
            // The move carried open tabs to their new paths; so too their
            // request-time seqs, for the check below to find them.
            let baseline: Vec<_> = pending
                .baseline
                .iter()
                .map(|(path, seq)| (moved_path(path, &pending.renames), *seq))
                .collect();
            if let Some(file) = self.stale_edit_target(&baseline, &edits) {
                self.status = format!(
                    "{moved_status}; references not updated: {file} changed while the server was working"
                );
            } else {
                match self.apply_rename_edits(&edits) {
                    Ok((files, _)) => {
                        self.status =
                            format!("{moved_status}, references updated in {files} file(s)");
                    }
                    Err(e) => {
                        self.status = format!("{moved_status}; could not update references: {e}")
                    }
                }
            }
        }
        if let Some(lsp) = self.lsp.as_mut() {
            lsp.did_rename_files(pending.renames);
        }
    }

    /// Perform an Explorer rename or move. Returns whether anything moved.
    pub(super) fn run_file_move(&mut self, op: FileMove) -> bool {
        match op {
            FileMove::Paste { dest_dir, paths } => {
                self.apply_paste_or_drop(&dest_dir, &paths, ExplorerClipMode::Cut)
            }
            FileMove::Rename {
                parent,
                old,
                new_name,
            } => match crate::widgets::file_tree::rename_in(&parent, &old, &new_name) {
                Ok(new_path) => {
                    self.status = format!("Renamed to {}", self.status_path(&new_path));
                    if let Some(idx) = self.tree.index_of_dir(&parent) {
                        self.tree.refresh_children(idx);
                        if let Some(new_idx) =
                            self.tree.nodes.iter().position(|n| n.path == new_path)
                        {
                            self.tree.select(new_idx);
                        }
                    }
                    self.rename_open_path_everywhere(&old, &new_path);
                    self.rename_review_boxes_path(&old, &new_path);
                    crate::history::move_path(&self.history_root, &old, &new_path);
                    self.sync_open_file_poll_mtime();
                    true
                }
                Err(e) => {
                    self.status = format!("Rename failed: {e}");
                    false
                }
            },
        }
    }

    /// Shared implementation for explorer paste and drag-drop. `mode`
    /// distinguishes a move (Cut/drag) from a copy (Copy/Alt-drag).
    /// Returns whether every item landed.
    pub(super) fn apply_paste_or_drop(
        &mut self,
        dest_dir: &Path,
        paths: &[PathBuf],
        mode: ExplorerClipMode,
    ) -> bool {
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        affected.insert(dest_dir.to_path_buf());
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for src in paths {
            let result = match mode {
                ExplorerClipMode::Cut => {
                    let parent = src
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.tree.root.clone());
                    affected.insert(parent);
                    crate::widgets::file_tree::move_into(dest_dir, src)
                }
                ExplorerClipMode::Copy => crate::widgets::file_tree::copy_into(dest_dir, src),
            };
            match result {
                Ok(p) => {
                    // Every tab, not just the active one: a background tab of
                    // a moved file kept the old path and recreated it on save.
                    if matches!(mode, ExplorerClipMode::Cut) {
                        self.rename_open_path_everywhere(src, &p);
                        self.rename_review_boxes_path(src, &p);
                        crate::history::move_path(&self.history_root, src, &p);
                    }
                    placed.push(p);
                }
                Err(e) => errors.push(format!("{}: {e}", src.display())),
            }
        }
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.tree.marked.clear();
        for p in &placed {
            self.tree.marked.insert(p.clone());
        }
        if let Some(first) = placed.first()
            && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
        {
            self.tree.selected = idx;
            self.tree.anchor = idx;
        }
        let verb = match mode {
            ExplorerClipMode::Cut => "Moved",
            ExplorerClipMode::Copy => "Copied",
        };
        if !errors.is_empty() {
            self.status = format!(
                "{verb} {}/{}; failed: {}",
                placed.len(),
                paths.len(),
                errors.join("; ")
            );
        } else if placed.len() == 1 {
            self.status = format!("{verb} 1 item to {}", self.status_path(dest_dir));
        } else {
            self.status = format!(
                "{verb} {} items to {}",
                placed.len(),
                self.status_path(dest_dir)
            );
        }
        errors.is_empty()
    }
}

/// Map a screen row to the tree row that should be highlighted as a drop
/// target. Hovering a directory targets that directory; hovering a file
/// targets its parent directory (so dragging onto a file row drops the
/// payload as siblings of that file). Returns `None` when the pointer is
/// over empty tree space, when the row would be a no-op (hovering an
/// item that's part of the drag set), or when the resolved directory is
/// inside one of the dragged folders.
pub(super) fn drag_target_index(
    tree: &crate::widgets::file_tree::FileTree,
    y: u16,
    drag_paths: &[PathBuf],
) -> Option<usize> {
    // The sticky band wins the row resolution: a drop on a pinned ancestor
    // targets THAT directory, not whatever row the band covers (#126 — files
    // used to land in the covered row's directory, silently).
    let idx = tree.sticky_row_at(y).or_else(|| tree.node_at_y(y))?;
    let node = tree.nodes.get(idx)?;
    let dir_idx = if node.is_dir {
        idx
    } else {
        let parent = node.path.parent()?;
        tree.index_of_dir(parent)?
    };
    let dir_path = tree.nodes.get(dir_idx)?.path.clone();
    for src in drag_paths {
        if crate::widgets::file_tree::is_descendant_or_same(&dir_path, src) {
            return None;
        }
        if let Some(parent) = src.parent()
            && parent == dir_path
            && drag_paths.len() == 1
        {
            // Dropping a single source onto its own parent is a no-op;
            // don't show a highlight so the user knows nothing will move.
            return None;
        }
    }
    Some(dir_idx)
}

pub(super) fn drop_target_dir(
    tree: &crate::widgets::file_tree::FileTree,
    target_idx: usize,
) -> PathBuf {
    tree.nodes
        .get(target_idx)
        .map(|n| n.path.clone())
        .unwrap_or_else(|| tree.root.clone())
}

/// Parse a bracketed-paste payload that originated from a Finder-style
/// drag-and-drop into the host terminal. Returns the absolute existing
/// paths the user dropped, or an empty Vec when the payload is plain text
/// (in which case the caller falls back to the normal paste path). Each
/// candidate token survives only if it resolves to an existing absolute
/// path on disk, so a stray paste of "/usr/bin" worth of typed text still
/// behaves like text.
pub fn parse_dropped_paths(s: &str) -> Vec<PathBuf> {
    parsed_drop_tokens(s)
        .into_iter()
        .filter(|p| p.is_absolute() && p.exists())
        .filter(|p| !is_iterm2_inline_image_drop(p))
        .collect()
}

/// Parse a bracketed-paste payload as candidate filesystem paths WITHOUT
/// requiring the paths to exist on this machine. Used by the remote-
/// launched croft to recognise drops whose paths refer to files on the
/// user's local Mac, which the relay will fetch over scp.
pub fn parse_foreign_dropped_paths(s: &str) -> Vec<PathBuf> {
    parsed_drop_tokens(s)
        .into_iter()
        .filter(|p| p.is_absolute())
        .filter(|p| !is_iterm2_inline_image_drop(p))
        .collect()
}

/// True when every token in a bracketed-paste payload normalises to an absolute
/// path (and there is at least one). This is the signature of a genuine Finder
/// drag, which carries only the dragged file path(s). A real drag does NOT move
/// keyboard focus to the Explorer tree, so a focus-only gate silently dropped
/// every drag where the user had last clicked the terminal or editor. A text
/// paste that merely *contains* a path — a shell command like `cat /etc/hosts`,
/// a pasted code snippet — always has at least one non-path token, so it returns
/// false and is left to land as ordinary text rather than being hijacked into a
/// relay pull that has no local file to fetch.
pub(super) fn is_pure_path_payload(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let tokens = shell_split_drop_payload(trimmed);
    if tokens.is_empty() {
        return false;
    }
    tokens.iter().all(|t| {
        normalise_dropped_token(t)
            .map(|p| p.is_absolute())
            .unwrap_or(false)
    })
}

/// True when the dropped path is iTerm2's temp file for an inline-image
/// drag. iTerm2 lets the user drag any OSC-1337 inline image out of the
/// terminal; on mouse-up it writes the image bytes to `$TMPDIR/iTerm2.<rand>.png`
/// and re-injects the path into the same terminal as a bracketed paste.
/// Croft renders the activity-bar icons, welcome wordmark, run-debug
/// hero, no-repo hero, and SSH empty-state hero as OSC-1337 image cells,
/// so a click-with-microdrag on any of those triggers iTerm2's drag-
/// handler and croft would otherwise open the resulting PNG as an image
/// preview tab or import it into the Explorer. Filtering these paths is
/// always safe — the user has no legitimate reason to drop iTerm2's own
/// temp image into croft, and matching by filename pattern is iTerm2-
/// specific enough that it cannot collide with real user assets.
pub(super) fn is_iterm2_inline_image_drop(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with("iTerm2.") || !name.ends_with(".png") {
        return false;
    }
    let middle = &name["iTerm2.".len()..name.len() - ".png".len()];
    if middle.is_empty() || !middle.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Defence in depth: only treat it as an iTerm2 drag artefact when
    // the file lives under a temp directory. macOS puts iTerm2's drag
    // temp under `/var/folders/.../TemporaryItems/` or `$TMPDIR`; both
    // resolve through `std::env::temp_dir()` ancestry. The starts_with
    // chain is forgiving because the path may be a symlinked variant
    // (e.g. `/private/var/...` vs `/var/...`).
    let parent = p.parent().unwrap_or(p);
    let parent_str = parent.to_string_lossy();
    parent_str.contains("/T/")
        || parent_str.contains("/TemporaryItems")
        || parent_str.starts_with("/tmp")
        || parent_str.starts_with("/var/folders")
        || parent_str.starts_with("/private/var/folders")
}

pub(super) fn parsed_drop_tokens(s: &str) -> Vec<PathBuf> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    shell_split_drop_payload(trimmed)
        .into_iter()
        .filter_map(|raw| normalise_dropped_token(&raw))
        .collect()
}

/// Split a Finder / iTerm2 drag-drop payload into individual path tokens
/// the way a POSIX-ish shell would. Backslash escapes the next char (so
/// `\ ` keeps a literal space inside a filename); single- and double-
/// quoted runs stay intact; unescaped whitespace (space, tab, CR, LF)
/// separates tokens. Quote/backslash characters are *retained* in the
/// emitted token so downstream `normalise_dropped_token` can reuse its
/// existing un-escape + un-quote logic.
pub(super) fn shell_split_drop_payload(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_double = false;
    let mut in_single = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && !in_single {
            // Carry the backslash and its escapee through verbatim;
            // `normalise_dropped_token` strips them later.
            cur.push('\\');
            if let Some(next) = chars.next() {
                cur.push(next);
            }
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            cur.push(c);
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            cur.push(c);
            continue;
        }
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') && !in_single && !in_double {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Strip surrounding quotes, decode `file://` URLs (with %xx escapes), and
/// unescape `\<space>` style backslash escapes that some shells / drag
/// sources produce. Anything that isn't a plausible filesystem path
/// returns None and is dropped by the caller.
pub(super) fn normalise_dropped_token(raw: &str) -> Option<PathBuf> {
    let mut s = raw.trim().to_string();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(rest) = s.strip_prefix("file://") {
        // Decode into BYTES and build the string once. A `%xx` escape is one
        // byte of a UTF-8 sequence, not a character; pushing it `as char`
        // read it as Latin-1 and opened a path that does not exist (#414).
        // A sequence that is not valid UTF-8 names no path this process can
        // open, so per this function's contract the token is dropped rather
        // than turned into a plausible-looking path of replacement
        // characters, which would pass every caller's "is this a path"
        // screen and open the wrong thing.
        let decoded = crate::shell_integration::percent_decode(rest.as_bytes());
        s = String::from_utf8(decoded).ok()?;
    }
    let mut unescaped = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.next()
        {
            unescaped.push(next);
            continue;
        }
        unescaped.push(c);
    }
    if unescaped.is_empty() {
        return None;
    }
    Some(PathBuf::from(unescaped))
}

/// Where `path` is after `renames` ran: under a moved file or folder, its
/// new place; anywhere else, unchanged.
pub(super) fn moved_path(path: &Path, renames: &[crate::lsp::manager::FileRenameOp]) -> PathBuf {
    renames
        .iter()
        .find_map(|r| path.strip_prefix(&r.old).ok().map(|rest| r.new.join(rest)))
        .map(|p| p.components().collect())
        .unwrap_or_else(|| path.to_path_buf())
}
