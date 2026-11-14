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
/// through [`build_tree_context_menu_items_in_dir`] with the dir it carries.
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

/// [`build_tree_context_menu_items`] reading the viewers under an explicit
/// config dir: the app passes the dir it carries, so the row it builds and
/// the click that resolves the row read the same extensions.
pub(super) fn build_tree_context_menu_items_in_dir(
    config_dir: &Path,
    node: Option<&crate::widgets::file_tree::Node>,
    root: &Path,
    selection: &[PathBuf],
    target_dir: &Path,
    clipboard: Option<&ExplorerClipboard>,
    compare_anchor: Option<&Path>,
) -> Vec<(String, MenuAction)> {
    build_tree_context_menu_items_with(
        node,
        root,
        selection,
        target_dir,
        clipboard,
        compare_anchor,
        |file| {
            crate::mcp::registry::viewer_for_path_in_dir(config_dir, file)
                .map(|v| (v.label.clone(), v.key()))
        },
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
        // Drain any COMMITS graph replies; ignore one for a since-left root.
        while let Ok((root, revspec, rows)) = self.graph_rx.try_recv() {
            self.graph_fetch_inflight = false;
            if root == self.active_scm_root && revspec == self.graph_revspec {
                self.commit_graph.set_rows(rows);
                changed = true;
            }
            // A trigger arrived while this reply was in flight: what it
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
            if !self.dep_ecosystems.is_empty()
                && self.explorer_views.is_visible(ExplorerView::Dependencies)
            {
                let tx = self.deps_tx.clone();
                std::thread::spawn(move || {
                    let deps = crate::widgets::dependencies::fetch_dependencies(&root);
                    let _ = tx.send(deps);
                });
            }
        }
        while let Ok(deps) = self.deps_rx.try_recv() {
            self.dependencies.set_deps(deps);
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
