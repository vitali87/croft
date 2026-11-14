//! Source Control: the panel and its keys, background git and its log, commits,
//! push, pull and sync, stash, branches and pickers, hunks, permalinks, the commit
//! graph, its dialogs, change gutters and inline blame.

use super::*;

/// How many commits the history scrubber loads up front (#371).
///
/// A ceiling rather than the whole history: the issue's own criterion is
/// that scrubbing 200 commits stays interactive, and a repo with 50,000 of
/// them should not pay for all of them to answer a question about the last
/// few hundred. Lazy loading around the cursor is the follow-up; this bounds
/// the first version so it cannot be slow on a large repo.
pub(super) const SCRUB_COMMIT_LIMIT: usize = 500;

/// Finished history-scrubber views kept for stepping back to (#371): a
/// few either side of the cursor. Each holds a whole file with its
/// highlighting, so this stays small.
pub(super) const SCRUB_VIEWS_KEPT: usize = 8;

/// Outlines of built history views kept while scrubbing (#371); cleared
/// wholesale past this, and rebuilt with each view the builder lands.
pub(super) const SCRUB_OUTLINES_KEPT: usize = 256;

/// What a share-link command does with the link (#1610).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShareLink {
    CopyPermalink,
    CopyHeadLink,
    OpenOnRemote,
}

/// What the COMMITS graph's right-click menu can do to a commit (#1627).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CommitAction {
    CherryPick,
    Revert,
    CheckoutDetached,
    CreateBranch,
    CreateTag,
    Reset(crate::git::ResetMode),
    CopyHash,
    CopyMessage,
}

/// What a network git operation's worker hands back: the UI-thread half
/// of the operation, applied to the app when it arrives.
pub(super) type GitNetDone = Box<dyn FnOnce(&mut App) + Send>;

/// A Source Control write that records the files on disk (#1438): a commit
/// variant or a stash. Held while the unsaved-changes prompt is open, and
/// across a format-on-save write, so the choice runs the request it
/// interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScmWrite {
    Commit,
    CommitStaged,
    CommitAll,
    Amend,
    CommitAndSync,
    CommitAndPush,
    Stash,
    StashIncludeUntracked,
    StashStaged,
    /// Stash & Checkout on a switch local changes blocked (#925).
    StashAndSwitch,
    /// Migrate Changes on a switch local changes blocked (#925).
    MigrateAndSwitch,
}

impl ScmWrite {
    pub(super) fn is_stash(self) -> bool {
        matches!(
            self,
            Self::Stash
                | Self::StashIncludeUntracked
                | Self::StashStaged
                | Self::StashAndSwitch
                | Self::MigrateAndSwitch
        )
    }

    /// The verb the prompt's buttons and status lines use.
    pub(super) fn verb(self) -> &'static str {
        if self.is_stash() { "Stash" } else { "Commit" }
    }
}

/// Why the branch picker is open, deciding what its Enter does. `Checkout`
/// is the default (click the branch name / "Checkout to"); the others are
/// reached from the Branch submenu and act on the highlighted *existing*
/// branch (or, for `CreateFrom`, use it as the base for a new branch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BranchPurpose {
    #[default]
    Checkout,
    CreateFrom,
    Delete,
    Merge,
    Rebase,
}

/// A COMMITS graph reply: the root and revspec it answers, and the rows.
/// Tagged rather than bare so a reply for a since-left root or a superseded
/// view is discarded on drain (#348).
pub(super) type GraphReply = (
    PathBuf,
    Option<String>,
    Vec<crate::widgets::commit_graph::GraphRow>,
);

pub(super) type GraphReplyRx = std::sync::mpsc::Receiver<GraphReply>;

pub(super) type GraphReplyTx = std::sync::mpsc::Sender<GraphReply>;

impl App {
    /// Re-query git status, but no more than once every ~400ms to avoid
    /// spawning a `git` process on every keystroke.  Called after the file
    /// watcher reports any changes.
    pub(super) fn refresh_git_status_debounced(&mut self) {
        let want_changes = self.sidebar_view == SidebarView::SourceControl;
        self.git.request_status_debounced(want_changes);
        for (_, w) in &mut self.git_extra {
            w.request_status_debounced(want_changes);
        }
    }

    /// Drain everything the git worker has shipped back since the last
    /// tick and apply each response to the matching cache (`git_status`
    /// for the status-bar badge, `source_control.entries` for the SC
    /// panel rows). Returns true iff anything was applied so the main
    /// loop knows to redraw. Cheap when the rx is empty: a single
    /// non-blocking try_recv. Called from `try_install_pending_init`,
    /// which already runs every tick.
    pub fn drain_git_responses(&mut self) -> bool {
        // The SCM surfaces follow the repository of the root owning the
        // focused file (#149): on a switch, seed the panel from that
        // worker's cached status and re-request its change list — the
        // non-active workers' lists are discarded at their drains below.
        let follow = self.active_workspace_root();
        if follow != self.scm_follow_root {
            // The user moved focus across folders: the panel resumes
            // following, releasing any overview pin (#161).
            self.scm_follow_root = follow.clone();
            self.scm_pin = None;
        }
        // A pin naming a folder that has since left the workspace would
        // re-seed a dead root forever (#162 review): drop it here so the
        // guard holds whichever path removed the folder.
        if self
            .scm_pin
            .as_ref()
            .is_some_and(|p| !self.roots.iter().any(|r| r == p))
        {
            self.scm_pin = None;
        }
        let active = self.scm_pin.clone().unwrap_or_else(|| follow.clone());
        if active != self.active_scm_root {
            self.seed_active_scm(active);
        }
        let test_root = self.followed_test_root(follow);
        if test_root != self.active_test_root {
            self.rebind_test_root(test_root);
        }
        // Ship any refresh the debounce window coalesced once the gap clears,
        // so the trailing edge of an edit burst lands without another FS event.
        let mut changed = false;
        self.take_default_branch();
        // Roots whose HEAD oid moved in this drain: the open Source Control
        // diffs for THESE re-read their HEAD blob (#471); the rest are
        // stamp-gated like any filesystem tick.
        let mut heads_moved: Vec<PathBuf> = Vec::new();
        let mut status_arrived = false;
        let primary_root = self.roots.primary().to_path_buf();
        let mut worker_roots: Vec<PathBuf> = vec![primary_root];
        worker_roots.extend(self.git_extra.iter().map(|(r, _)| r.clone()));
        for ws_root in worker_roots {
            let is_active = ws_root == self.active_scm_root;
            // Split the borrows: the panel is only handed to the active
            // repo's worker.
            let panel = &mut self.source_control;
            let w = match self.git_extra.iter_mut().find(|(r, _)| *r == ws_root) {
                Some((_, w)) => w,
                None => &mut self.git,
            };
            w.flush_pending();
            let this_changed = w.drain_into(is_active.then_some(panel));
            // A completed refresh whose status came back identical is still
            // news to an open staged / branch / previous-commit diff (#471):
            // those read the index or a ref, which porcelain status does
            // not fully describe.
            if w.take_status_arrival() {
                status_arrived = true;
            }
            if !this_changed {
                continue;
            }
            changed = true;
            let status = w.status().clone();
            if is_active && status.in_repo && self.default_branch_label.is_none() {
                self.ask_default_branch();
            }
            // HEAD moved in THIS repo (commit, checkout, pull): the cached
            // gutter baselines for ITS files are stale, as is blame; the
            // COMMITS graph refetches only for the active repo.
            let oid = status.head_oid.clone();
            if self.git_head_oids.get(&ws_root) != Some(&oid) {
                self.git_head_oids.insert(ws_root.clone(), oid);
                // The diff tag stores `scm_root()`, the git TOPLEVEL, which
                // is not the workspace root for a subdirectory workspace or
                // for a root reached through a symlink (git canonicalises
                // `--show-toplevel`). Record what the tag will compare with.
                heads_moved.push(status.repo_root.clone().unwrap_or_else(|| ws_root.clone()));
                if is_active {
                    self.refresh_commit_graph();
                }
                for group in std::iter::once(&mut self.editor)
                    .chain(self.editor_layout.inactive_groups_mut())
                {
                    for ed in &mut group.editors {
                        let owned = ed
                            .path
                            .as_deref()
                            .and_then(|p| self.roots.owning_root(p))
                            .is_some_and(|r| r == ws_root);
                        if owned {
                            ed.git_baseline_for = None;
                        }
                    }
                }
                self.blame_fetched = None;
            }
        }
        if changed {
            self.refresh_scm_change_badge();
            // The overview rows track the same drains that feed the badge
            // (#162 review: sync_focus_flags runs on focus TRANSITIONS,
            // not per tick, so background-repo updates lagged).
            self.refresh_scm_repositories();
            // The Explorer dims git-ignored rows across EVERY root: each
            // worker's set is cwd-scoped to its root already, so the union
            // is exact. Single-root keeps the Arc clone (no set copy).
            if self.git_extra.is_empty() {
                self.tree.ignored = self.git.status().ignored.clone();
                self.tree.git_decorations = self.git.status().decorations.clone();
            } else {
                let mut union: std::collections::HashSet<PathBuf> =
                    self.git.status().ignored.as_ref().clone();
                let mut decorations = self.git.status().decorations.as_ref().clone();
                for (_, w) in &self.git_extra {
                    union.extend(w.status().ignored.iter().cloned());
                    // Roots can nest (a repo folder inside another root's
                    // tree), so a shared path keeps its strongest status.
                    for (path, &dec) in w.status().decorations.iter() {
                        let slot = decorations.entry(path.clone()).or_insert(dec);
                        *slot = (*slot).max(dec);
                    }
                }
                self.tree.ignored = std::sync::Arc::new(union);
                self.tree.git_decorations = std::sync::Arc::new(decorations);
            }
        }
        // A completed status refresh means HEAD or the index may have moved:
        // a Source Control diff re-reads its HEAD blob when its root's HEAD
        // oid changed, a staged / branch / previous-commit view re-runs its
        // `git diff` on every arrival (#471). This is the debounced cadence,
        // so a burst of writes costs one re-run, and a rebuilt view owes a
        // redraw even when the status itself was unchanged.
        let diffs_refreshed = (changed || status_arrived)
            && self.refresh_open_diff_views(true, &heads_moved, &std::collections::BTreeSet::new());
        changed || diffs_refreshed
    }

    /// Resolve the active repository's default branch on a thread of its
    /// own, unless that is already under way; [`Self::take_default_branch`]
    /// files the answer.
    pub(super) fn ask_default_branch(&mut self) {
        let root = self.active_scm_root.clone();
        if self
            .default_branch_ask
            .as_ref()
            .is_some_and(|(r, _)| *r == root)
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::default_branch(&asked).ok());
        });
        self.default_branch_ask = Some((root, rx));
    }

    /// File the default branch [`Self::ask_default_branch`] resolved, if it
    /// is in and is for the repository still active.
    pub(super) fn take_default_branch(&mut self) {
        let Some((root, rx)) = self.default_branch_ask.as_ref() else {
            return;
        };
        let branch = match rx.try_recv() {
            Ok(branch) => branch,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        if *root == self.active_scm_root && self.default_branch_label.is_none() {
            self.default_branch_label = branch;
        }
        self.default_branch_ask = None;
    }

    /// Point the SCM surfaces at `active`'s repository: seed the panel
    /// from its worker's cached status, drop the outgoing repo's change
    /// list (rendering another repository's entries under this branch
    /// header would be wrong; the fresh list lands one worker roundtrip
    /// later), re-request it past the debounce, and refetch the COMMITS
    /// graph. Shared by the focus-follow switch and the remove-folder
    /// fallback (#150 review: assigning the field directly left the
    /// removed repo's panel on screen, since the switch-detect saw no
    /// difference).
    pub(super) fn seed_active_scm(&mut self, active: PathBuf) {
        self.active_scm_root = active.clone();
        self.default_branch_label = None;
        // A lane diff belongs to the root it was asked for. Carried into
        // another repo the range names a branch that does not exist there,
        // and `branch_history_range` answers an unresolvable range with an
        // EMPTY vec rather than an error - so the panel would show a repo
        // with commits as having none, with nothing on screen saying why.
        self.graph_revspec = None;
        let w = self.git_worker_for_root_mut(&active);
        w.bypass_debounce();
        w.request_status_and_changes();
        let status = self.git_worker_for_root(&active).status().clone();
        self.source_control.set_status(status, Vec::new());
        self.refresh_scm_repositories();
        self.refresh_commit_graph();
    }

    /// Feed the Source Control empty state the repos one folder below the
    /// active root when that root is in none (#1227), so it offers to open
    /// them rather than `git init` around them. A depth-1 `read_dir`,
    /// rerun at most every two seconds while the card shows.
    pub(super) fn sync_nested_repos(&mut self) {
        if self.source_control.status.in_repo {
            self.source_control.nested_repos.clear();
            self.nested_repo_scan = None;
            return;
        }
        let root = self.active_scm_root.clone();
        let fresh = self
            .nested_repo_scan
            .as_ref()
            .is_some_and(|(r, at)| *r == root && at.elapsed() < Duration::from_secs(2));
        if fresh {
            return;
        }
        self.source_control.nested_repos = crate::git::child_repos(&root)
            .into_iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some((format!("{name}/"), p))
            })
            .collect();
        self.nested_repo_scan = Some((root, std::time::Instant::now()));
    }

    /// The git worker for a workspace root: the primary's, or the
    /// matching secondary's (#149).
    pub(super) fn git_worker_for_root(&self, ws_root: &Path) -> &GitWorker {
        self.git_extra
            .iter()
            .find(|(r, _)| r == ws_root)
            .map(|(_, w)| w)
            .unwrap_or(&self.git)
    }

    pub(super) fn git_worker_for_root_mut(&mut self, ws_root: &Path) -> &mut GitWorker {
        match self.git_extra.iter().position(|(r, _)| r == ws_root) {
            Some(i) => &mut self.git_extra[i].1,
            None => &mut self.git,
        }
    }

    /// The active repository's worker — what `self.git` meant while
    /// there was one root (#149).
    pub(super) fn scm_worker(&self) -> &GitWorker {
        self.git_worker_for_root(&self.active_scm_root)
    }

    /// Rebuild the multi-root repositories overview rows (#161) from the
    /// per-folder workers: called from the git drain (fresh statuses),
    /// the focus sync, and the seed (active-highlight moves with a row
    /// click). Empty — section hidden — with one folder.
    pub(super) fn refresh_scm_repositories(&mut self) {
        self.source_control.repositories = if self.roots.is_multi() {
            let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
            let labels = crate::workspace::root_display_labels(&roots);
            roots
                .iter()
                .zip(labels)
                .filter_map(|(ws, label)| {
                    let status = self.git_worker_for_root(ws).status();
                    if !status.in_repo {
                        return None;
                    }
                    Some(crate::widgets::source_control::RepoRow {
                        label,
                        branch: status.branch.clone(),
                        changes: status.changed_count,
                        active: *ws == self.active_scm_root,
                        workspace_root: ws.clone(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    /// Let the ACTIVE repo's next debounced refresh through immediately —
    /// every Source Control mutation runs against the active repo (#149).
    pub(super) fn active_git_bypass_debounce(&mut self) {
        let ws = self.active_scm_root.clone();
        self.git_worker_for_root_mut(&ws).bypass_debounce();
    }

    /// Keep each text tab's git-gutter baseline current. The first time a tab
    /// is seen (or after it switches files, or after HEAD moves), shell out
    /// once for the committed version of the file and hand it to the editor;
    /// the per-line diff is then recomputed in-buffer on every edit with no
    /// further subprocess. Cheap no-op once every open tab has a baseline:
    /// just a path compare per tab.
    ///
    /// `read_file_at_head` is a `git show` (~tens of ms) but runs at most once
    /// per (file, HEAD) pair, never on the keystroke path. Baselines are shared
    /// across groups within a sweep: a split usually holds the SAME file in two
    /// panes, and a HEAD move invalidates every tab in every group at once, so
    /// without the cache one tick would shell out once per tab per group.
    /// ponytail: synchronous fetch on the tick. Move to the git worker thread
    /// if opening a file in a huge repo ever visibly hitches.
    pub(super) fn sync_git_gutters(&mut self) {
        // `HEAD:<rel>` pathspecs resolve against the toplevel (#139), and
        // each file resolves against the repo of ITS owning workspace
        // root (#149). Precompute (workspace root → repo toplevel) so the
        // editor loop below borrows nothing else off self.
        // A root in no repo (or with no status reply yet) maps to `None`,
        // and the file's own repo is asked instead: a workspace opened
        // above its repo still gets gutters (#1227).
        let repo_bases: Vec<(PathBuf, Option<PathBuf>)> = self
            .roots
            .iter()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>()
            .into_iter()
            .map(|ws| {
                let repo = self.git_worker_for_root(&ws).status().repo_root.clone();
                (ws, repo)
            })
            .collect();
        let repo_for = |path: &Path| -> Option<PathBuf> {
            repo_bases
                .iter()
                .filter(|(ws, _)| path.starts_with(ws))
                .max_by_key(|(ws, _)| ws.components().count())
                .and_then(|(_, repo)| repo.clone())
                .or_else(|| path.parent().and_then(crate::git::repo_toplevel))
        };
        // Keyed by encoding too: the same file open in two tabs under two
        // encodings needs HEAD decoded each tab's way (#1242).
        let mut fetched: std::collections::HashMap<
            (PathBuf, &str),
            Option<crate::packed_lines::PackedLines>,
        > = std::collections::HashMap::new();
        let groups =
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut());
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            let Some(path) = ed.path.clone() else {
                continue;
            };
            if ed.git_baseline_for.as_deref() == Some(path.as_path()) {
                continue;
            }
            // Untracked files / non-repo dirs yield Err here, so the gutter
            // simply shows nothing; the baseline-for marker still updates so we
            // don't re-shell every tick.
            let enc = ed.encoding;
            let head = fetched
                .entry((path.clone(), enc.name()))
                .or_insert_with(|| {
                    let root = repo_for(&path)?;
                    path.strip_prefix(&root)
                        .ok()
                        .and_then(|rel| rel.to_str())
                        .and_then(|rel| crate::git::read_bytes_at_head(&root, rel).ok())
                        .and_then(|bytes| crate::widgets::editor::decode_blob(&bytes, enc))
                        .map(|text| text.lines().collect())
                })
                .clone();
            ed.set_git_head_lines(path, head);
        }
    }

    /// Fetch per-line git blame for the active editor off-thread, once per
    /// (file, HEAD). Mirrors the TIMELINE fetch: a marker gates the spawn so
    /// it fires exactly once per active-file change, and a HEAD move resets
    /// the marker (see the git-drain path) so blame refreshes after a commit.
    /// Gated on the pref so a subprocess never runs when blame is off.
    pub(super) fn sync_blame(&mut self) {
        self.editor.blame_enabled = self.inline_blame_enabled;
        self.editor.provenance_overlay = self.provenance_overlay;
        self.editor.auto_close_pairs = self.auto_close_pairs_for(editor_scope_id(&self.editor));
        self.editor.overwrite = self.overwrite_mode;
        self.editor.whitespace_mode = whitespace_mode_in(
            &self.language_prefs,
            self.whitespace_mode,
            editor_scope_id(&self.editor),
        );
        if !self.inline_blame_enabled {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.blame_fetched = None;
            return;
        };
        if self.blame_fetched.as_deref() == Some(path.as_path()) {
            return;
        }
        self.blame_fetched = Some(path.clone());
        // Pair `-C` and the pathspec with the OWNING workspace root
        // (#149): a secondary root's file blames within its own repo;
        // the cwd-relative pathspec stays consistent by construction.
        let root = self
            .roots
            .owning_root(&path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        let Some(rel) = path
            .strip_prefix(&root)
            .ok()
            .and_then(|r| r.to_str())
            .map(String::from)
        else {
            // Outside the repo: no blame, but the marker is set so we don't
            // respawn every tick.
            self.editor.set_blame(path, None);
            return;
        };
        let tx = self.blame_tx.clone();
        std::thread::spawn(move || {
            let blame = crate::git::blame(&root, &rel);
            let _ = tx.send((path, blame));
        });
    }

    /// Install any blame reply whose file is still the active editor. An
    /// empty result (untracked file, git unavailable) clears the annotation.
    /// Returns true when a reply landed so the frame redraws with it.
    pub(super) fn drain_blame(&mut self) -> bool {
        let mut changed = false;
        while let Ok((path, blame)) = self.blame_rx.try_recv() {
            if self.editor.path.as_deref() == Some(path.as_path()) {
                let data = if blame.is_empty() { None } else { Some(blame) };
                self.editor.set_blame(path, data);
                changed = true;
            }
        }
        changed
    }

    /// The caret line's commit as the inline blame names it (#1556), full
    /// hash and short; when it names none, the status line says why.
    pub(super) fn caret_line_commit(&mut self) -> Option<(String, String)> {
        use crate::widgets::editor::LineBlame;
        let line = self.editor.cursor_row + 1;
        self.status = match self.editor.cursor_line_blame() {
            Some(LineBlame::Commit(c)) => {
                return Some((c.hash.clone(), c.short_hash().to_string()));
            }
            Some(LineBlame::Uncommitted) => format!("Line {line} has uncommitted changes"),
            None if !self.inline_blame_enabled => {
                String::from("Inline blame is off: Git: Toggle Inline Blame turns it on")
            }
            None => format!("Git has no commit for line {line}"),
        };
        None
    }

    /// Git: Open Commit for Line, and a click on the inline blame (#1556):
    /// the changes the caret line's commit made to this file, in the
    /// side-by-side diff a TIMELINE row opens (GitLens's Open Changes).
    pub(super) fn open_commit_for_line(&mut self) {
        if let Some((hash, _)) = self.caret_line_commit()
            && let Some(path) = self.editor.path.clone()
        {
            self.open_timeline_diff(path, hash);
        }
    }

    /// The pointer rested on the inline blame naming `hash` (#1556): fetch
    /// the commit's card off the UI thread, for [`Self::drain_blame_hover`].
    pub(super) fn hover_blame_card(&mut self, col: u16, row: u16, hash: String) {
        let shown = self.hover_popup.is_some();
        if shown && self.blame_hover.as_ref().is_some_and(|(h, _)| *h == hash) {
            return;
        }
        // The file's own folder is in its repository, as blame's is.
        let Some(dir) = self.editor.path.as_deref().and_then(Path::parent) else {
            return;
        };
        let dir = dir.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let job = hash.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::commit_card(&dir, &job));
        });
        // A word's hover still in flight must not land over the card.
        self.hover_request_id = None;
        self.hover_word = None;
        self.hover_anchor = Some((col, row));
        self.blame_hover = Some((hash, rx));
    }

    /// Show the inline blame's commit card once `git log` has it, if the
    /// pointer is still on an annotation naming that commit: a tab switch or
    /// caret move while git ran can put another commit's there. True when
    /// the card appeared.
    pub(super) fn drain_blame_hover(&mut self) -> bool {
        let Some((hash, card)) = self
            .blame_hover
            .as_ref()
            .and_then(|(hash, rx)| rx.try_recv().ok().map(|card| (hash.clone(), card)))
        else {
            return false;
        };
        let under = self
            .hover
            .cell()
            .and_then(|(c, r)| self.editor.blame_commit_at(c, r));
        if under != Some(hash.as_str()) {
            self.blame_hover = None;
            return false;
        }
        let card = card.unwrap_or_else(|e| format!("Could not read the commit: {e}"));
        self.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
            format!("{card}\n\nClick to open its changes (Git: Open Commit for Line)"),
            self.hover_anchor.unwrap_or((0, 0)),
        ));
        true
    }

    pub(super) fn render_commit_dropdown(&mut self, frame: &mut ratatui::Frame) {
        use crate::widgets::source_control::CommitMenuItem;
        // The dropdown belongs to the Source Control view only. Gate on it
        // so a stale `commit_menu_open` flag can never paint the menu over
        // another sidebar view (e.g. Run-Debug) after a view switch.
        if !self.commit_menu_open || self.sidebar_view != SidebarView::SourceControl {
            self.source_control.commit_menu_item_areas.clear();
            return;
        }
        let caret = self.source_control.last_commit_caret_area;
        let main = self.source_control.last_button_area;
        if caret.width == 0 || main.width == 0 {
            return;
        }
        let default_branch = self
            .default_branch_label
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let items: Vec<(CommitMenuItem, String)> = CommitMenuItem::ALL
            .iter()
            .map(|item| (*item, item.label(&default_branch)))
            .collect();
        // Row layout: [pad][icon][gap][label][pad]. The icon occupies one
        // cell at item_x+1; the label starts two cells later at item_x+3.
        const ICON_PAD: u16 = 3;
        let label_w = items
            .iter()
            .map(|(_, l)| l.chars().count() as u16 + ICON_PAD + 1)
            .max()
            .unwrap_or(0);
        let item_w: u16 = label_w.max(caret.width);
        let item_x = (caret.x + caret.width).saturating_sub(item_w).max(main.x);
        let item_y_top = caret.y + caret.height;
        self.source_control.commit_menu_item_areas.clear();
        if let Some(t) = crate::widgets::surface::tones(self.theme) {
            // A raised card with the accent lip, one row taller above and
            // below than the rows, whose rim falls on the rows' pad cells:
            // each hit rect is still the row drawn. Icons rest muted; the
            // row under the pointer lifts, its icon and label with it.
            let card = Rect {
                x: item_x,
                y: item_y_top,
                width: item_w,
                height: items.len() as u16 + 2,
            }
            .intersection(frame.area());
            if card.width < 3 || card.height < 3 {
                return;
            }
            let pointer = self.pointer_cell;
            let buf = frame.buffer_mut();
            ratatui::widgets::Widget::render(ratatui::widgets::Clear, card, buf);
            buf.set_style(card, Style::default().bg(t.raised()));
            crate::widgets::surface::paint_overlay_frame(buf, card, self.theme);
            for (i, (item, label)) in items.into_iter().enumerate() {
                let row_y = card.y + 1 + i as u16;
                if row_y + 1 >= card.bottom() {
                    break;
                }
                let row_rect = Rect::new(card.x, row_y, card.width, 1);
                let lit = crate::widgets::hover::contains(row_rect, pointer);
                if lit {
                    let inner = Rect::new(card.x + 1, row_y, card.width - 2, 1);
                    buf.set_style(inner, Style::default().bg(t.hover_on(t.raised)));
                }
                let (icon_fg, ink) = if lit {
                    (t.text(), t.strong())
                } else {
                    (t.muted(), t.text())
                };
                buf.set_string(
                    card.x + 1,
                    row_y,
                    item.icon().to_string(),
                    Style::default().fg(icon_fg),
                );
                buf.set_stringn(
                    card.x + 3,
                    row_y,
                    &label,
                    usize::from(card.width.saturating_sub(4)),
                    Style::default().fg(ink),
                );
                self.source_control
                    .commit_menu_item_areas
                    .push((row_rect, item));
            }
            return;
        }
        let menu_rect = Rect {
            x: item_x,
            y: item_y_top,
            width: item_w,
            height: items.len() as u16,
        };
        let bg = self.theme.ui(Color::Rgb(0x25, 0x2b, 0x37));
        let fg = self.theme.ui(Color::White);
        let style = Style::default().fg(fg).bg(bg);
        frame.render_widget(ratatui::widgets::Clear, menu_rect);
        for (i, (item, label)) in items.into_iter().enumerate() {
            let row_y = item_y_top + i as u16;
            let row_rect = Rect {
                x: item_x,
                y: row_y,
                width: item_w,
                height: 1,
            };
            for rx in 0..row_rect.width {
                frame.buffer_mut()[(row_rect.x + rx, row_rect.y)]
                    .set_symbol(" ")
                    .set_style(style);
            }
            // Leading codicon, colored per action: push glyphs in blue,
            // diff/compare glyphs in purple, mirroring VS Code's SCM accents.
            let icon_color = match item {
                CommitMenuItem::CommitAndPush => self.theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2)),
                CommitMenuItem::Push | CommitMenuItem::Pull | CommitMenuItem::Sync => {
                    self.theme.ui(Color::Rgb(0x6c, 0xb6, 0xff))
                }
                CommitMenuItem::CheckoutBranch => self.theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)),
                CommitMenuItem::Stash | CommitMenuItem::StashPop => {
                    self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))
                }
                CommitMenuItem::ViewStagedDiff
                | CommitMenuItem::ViewPreviousCommitDiff
                | CommitMenuItem::ViewDefaultBranchDiff => {
                    self.theme.ui(Color::Rgb(0xc0, 0xa4, 0xf5))
                }
            };
            frame.buffer_mut().set_string(
                row_rect.x + 1,
                row_rect.y,
                item.icon().to_string(),
                Style::default().fg(icon_color).bg(bg),
            );
            frame
                .buffer_mut()
                .set_string(row_rect.x + 3, row_rect.y, &label, style);
            self.source_control
                .commit_menu_item_areas
                .push((row_rect, item));
        }
    }

    pub(super) fn dispatch_commit_menu_item(
        &mut self,
        item: crate::widgets::source_control::CommitMenuItem,
    ) {
        use crate::widgets::source_control::CommitMenuItem;
        match item {
            CommitMenuItem::CommitAndPush => self.commit_and_push_source_control(),
            CommitMenuItem::Push => self.push_source_control(),
            CommitMenuItem::Pull => self.pull_source_control(),
            CommitMenuItem::Sync => self.sync_source_control(),
            CommitMenuItem::CheckoutBranch => self.open_branch_picker(),
            CommitMenuItem::Stash => self.stash_source_control(),
            CommitMenuItem::StashPop => self.stash_pop_source_control(),
            CommitMenuItem::ViewStagedDiff => self.view_staged_diff_source_control(),
            CommitMenuItem::ViewPreviousCommitDiff => {
                self.view_previous_commit_diff_source_control()
            }
            CommitMenuItem::ViewDefaultBranchDiff => self.view_default_branch_diff_source_control(),
        }
    }

    pub(super) fn render_discard_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pd) = self.pending_discard.as_ref() else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        // Clipped to the frame: a terminal narrower or shorter than the
        // dialog's minimum made a rect outside the buffer, which panics.
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                format!(" {} ", crate::i18n::tr("DISCARD CHANGES?")),
                Style::default()
                    .fg(Color::White)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let warn_text = if pd.untracked {
            crate::i18n::tr("This will permanently delete the file from disk.")
        } else {
            crate::i18n::tr("This will overwrite your local changes from HEAD.")
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                warn_text,
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                truncate_for_display(&pd.rel_path, inner.width as usize),
                Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es, discard   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    /// Confirmation modal for Revert Hunk: destroys the uncommitted edit
    /// under the diff caret, so it gets the same red Y/N gate as discard.
    pub(super) fn render_revert_hunk_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pr) = self.pending_revert_hunk.as_ref() else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " REVERT HUNK? ",
                Style::default()
                    .fg(Color::White)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                "This will undo the change hunk under the cursor on disk.",
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                truncate_for_display(&pr.rel_path, inner.width as usize),
                Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es, revert   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    /// The unsaved-changes prompt in front of a commit or stash (#1438):
    /// names the unsaved files and offers Save All & Commit, Commit Anyway
    /// or Cancel, as VS Code does.
    pub(super) fn render_unsaved_scm_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pending) = self.pending_unsaved_scm.as_ref() else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 8;
        let rect = Rect {
            x: (area.width.saturating_sub(width)) / 2 + area.x,
            y: (area.height.saturating_sub(height)) / 2 + area.y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let accent = self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b));
        let verb = pending.op.verb();
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                format!(" {} ", crate::i18n::tr("UNSAVED CHANGES")),
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let n = pending.files.len();
        let names: Vec<String> = pending
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&pending.root)
                    .unwrap_or(p)
                    .display()
                    .to_string()
            })
            .collect();
        let white = Style::default().fg(self.theme.ui(Color::White));
        let dim = Style::default().fg(self.theme.ui(Color::Rgb(0x9a, 0xa4, 0xb8)));
        let key = Style::default().fg(accent).add_modifier(Modifier::BOLD);
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                format!(
                    "{n} file{} {} unsaved changes: {}",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "has" } else { "have" },
                    names.join(", ")
                ),
                white,
            )),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                format!("{verb} records the files on disk, without these edits."),
                dim,
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled("[S]", key),
                ratatui::text::Span::raw(format!(" Save All & {verb}   ")),
                ratatui::text::Span::styled("[C]", key),
                ratatui::text::Span::raw(format!(" {verb} Anyway   ")),
                ratatui::text::Span::styled("[Esc]", key),
                ratatui::text::Span::raw(" Cancel"),
            ]),
        ]);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: false }),
            inner,
        );
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            accent,
        );
    }

    /// Confirmation modal for "Discard All Changes" — reverts every tracked
    /// edit to HEAD, so it gets the same red Y/N gate as a single discard.
    pub(super) fn render_discard_all_confirm(&self, frame: &mut ratatui::Frame) {
        if !self.pending_discard_all {
            return;
        }
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 7;
        let rect = Rect {
            x: (area.width.saturating_sub(width)) / 2 + area.x,
            y: (area.height.saturating_sub(height)) / 2 + area.y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " DISCARD ALL CHANGES? ",
                Style::default()
                    .fg(Color::White)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                "This reverts every tracked file to HEAD. Untracked files are kept.",
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es, discard all   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    /// Confirmation modal for Drop Stash (#1128), in the Discard All style.
    /// Only Y drops: the pick that opened it was an Enter, and a second
    /// stray one must not delete the stash.
    pub(super) fn render_stash_drop_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pending) = &self.pending_stash_drop else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 8;
        let rect = Rect {
            x: (area.width.saturating_sub(width)) / 2 + area.x,
            y: (area.height.saturating_sub(height)) / 2 + area.y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " DROP STASH? ",
                Style::default()
                    .fg(Color::White)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let white = Style::default().fg(self.theme.ui(Color::White));
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                pending.label.as_str(),
                white.add_modifier(Modifier::BOLD),
            )),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                "This permanently deletes the stash.",
                white,
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es, drop it   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    pub(super) fn handle_source_control_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc) {
            self.set_sidebar_view(SidebarView::Explorer);
            return;
        }
        let cmd_or_ctrl = key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+Enter: commit & push. We deliberately match ONLY Control
        // here (not Super). iTerm2's default keymap binds Cmd+Enter to
        // "Toggle Fullscreen" and never delivers the chord to the TUI,
        // which made the original Cmd+Enter wiring feel inert; Ctrl is
        // unintercepted across every terminal we target.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Enter) {
            self.commit_and_push_source_control();
            return;
        }
        // Cmd+A: select every entry row in the panel so a follow-up Cmd+S
        // stages them all in one go. Distinct from the editor's Cmd+A
        // (select-all text) which the user kept hitting in the SC pane
        // and getting the wrong scope.
        if cmd_or_ctrl && matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A')) {
            let n = self.source_control.select_all_changes();
            self.status = format!("Selected {n} change(s)");
            return;
        }
        // Cmd+S: stage the selected entry, or every entry in the
        // multi-selection. Mirrors clicking the inline "+" icon but
        // sweeps across the whole selection.
        if cmd_or_ctrl && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
            self.stage_selected_source_control_entries();
            return;
        }
        if self.source_control.list_focused {
            self.handle_source_control_list_key(key);
            return;
        }
        if is_clipboard_paste_key(key) {
            if let Some(text) = self.clipboard_text() {
                self.source_control.insert_str(&text);
                self.poke_cursor();
            }
            return;
        }
        match key.code {
            KeyCode::Backspace => {
                self.source_control.backspace();
                self.poke_cursor();
            }
            KeyCode::Delete => {
                self.source_control.delete_char();
                self.poke_cursor();
            }
            KeyCode::Left => {
                self.source_control.move_cursor_left();
                self.poke_cursor();
            }
            KeyCode::Right => {
                self.source_control.move_cursor_right();
                self.poke_cursor();
            }
            // Home/End stay on the caret's line; with Ctrl they take the
            // whole message (#1334).
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.source_control.message_start();
                self.poke_cursor();
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.source_control.message_end();
                self.poke_cursor();
            }
            KeyCode::Home => {
                self.source_control.home();
                self.poke_cursor();
            }
            KeyCode::End => {
                self.source_control.end();
                self.poke_cursor();
            }
            // In a multi-line message Up/Down move between its lines; on
            // its first line Up scrolls the change list, and on its last
            // line Down steps into the list (#871), as Tab does.
            KeyCode::Up => {
                if self.source_control.move_cursor_up() {
                    self.poke_cursor();
                } else {
                    self.source_control.scroll_up(1);
                }
            }
            KeyCode::Down => {
                if self.source_control.move_cursor_down() {
                    self.poke_cursor();
                } else if !self.source_control.focus_list() {
                    self.source_control.scroll_down(1);
                }
            }
            KeyCode::Tab => {
                self.source_control.focus_list();
            }
            // Shift/Alt+Enter start a new message line (the commit body),
            // VS Code's multi-line SCM input; plain Enter commits.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.source_control.insert_char('\n');
                self.poke_cursor();
            }
            KeyCode::Enter => self.commit_source_control(),
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                self.source_control.insert_char(c);
                self.poke_cursor();
            }
            _ => {}
        }
    }

    /// Keys while the change list holds the keyboard (#871): Up/Down move
    /// the selection, Enter opens its diff, Space or `s` stages it, `u`
    /// unstages it, and Backspace, Delete or `d` asks before discarding it.
    /// Tab, or Up from the first row, returns to the message box.
    pub(super) fn handle_source_control_list_key(&mut self, key: KeyEvent) {
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let selected = self.source_control.selected_change.filter(|_| !has_mod);
        // Stage acts on a row outside Staged and Unstage on a Staged one, as
        // the mouse's row actions do: a partly staged file has a row in
        // each, and `git add` from its Staged row (or `git reset` from its
        // Changes row) would undo the half the user kept apart.
        let staged = selected
            .and_then(|i| self.source_control.entries.get(i))
            .is_some_and(|e| e.kind.section() == crate::git::ChangeSection::Staged);
        let sc = &mut self.source_control;
        match (key.code, selected) {
            (KeyCode::Tab | KeyCode::BackTab, _) => sc.list_focused = false,
            (KeyCode::Up, _) => sc.list_focused = sc.move_selection(true),
            (KeyCode::Down, _) => {
                sc.move_selection(false);
            }
            (KeyCode::Enter, Some(idx)) => self.open_source_control_entry(idx),
            (KeyCode::Char(' ' | 's'), Some(idx)) if !staged => {
                self.stage_source_control_entry(idx)
            }
            (KeyCode::Char('u'), Some(idx)) if staged => self.unstage_source_control_entry(idx),
            (KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('d'), Some(idx)) => {
                self.request_discard_source_control_entry(idx)
            }
            _ => {}
        }
    }

    pub(super) fn refresh_source_control(&mut self) {
        // Non-blocking: post a StatusAndChanges request and let the worker
        // reply via `git_response_rx`. The panel paints immediately from
        // whatever entries the last drain installed (typically <400 ms
        // old via the FS-watcher tick); the fresh state lands within one or
        // two frames after the click. StatusAndChanges (not just Changes) so
        // the branch sync counts (`↑1` / `↓N`) and dirty flag refresh too —
        // a commit changes those without changing the file list. Still off
        // the UI thread, so no synchronous `git status` stall.
        let ws = self.active_scm_root.clone();
        self.git_worker_for_root_mut(&ws)
            .request_status_and_changes();
        self.refresh_commit_graph();
    }

    /// Refetch the COMMITS graph off-thread: `git log --topo-order` plus the
    /// pure lane layout both run on the fetch thread, and the finished rows
    /// land in [`graph_rx`] tagged with the root they describe. The inflight
    /// latch collapses a burst of triggers into one shell-out plus one
    /// queued refetch: a trigger during flight is answering a question the
    /// in-flight fetch predates (a Make Root, a newer HEAD), so it must
    /// re-fire when the reply drains rather than be dropped — a root change
    /// racing the fetch otherwise left the panel on "Loading" until HEAD
    /// next moved.
    pub(super) fn refresh_commit_graph(&mut self) {
        if self.graph_fetch_inflight {
            self.graph_refetch_queued = true;
            return;
        }
        self.graph_fetch_inflight = true;
        // The COMMITS section follows the active repo (#149).
        let root = self.active_scm_root.clone();
        let revspec = self.graph_revspec.clone();
        let tx = self.graph_tx.clone();
        std::thread::spawn(move || {
            let commits = match revspec.as_deref() {
                Some(spec) => crate::git::branch_history_range(&root, spec, 400),
                None => crate::git::commit_graph(&root, 400),
            };
            let rows = crate::widgets::commit_graph::layout_graph(commits);
            let _ = tx.send((root, revspec, rows));
        });
    }

    /// View Stash (#1581): stash `index`'s changes against the commit it
    /// was made on, untracked files included, in a read-only scratch tab
    /// like a commit's patch.
    pub(super) fn open_stash_patch(&mut self, index: usize) {
        let root = self.scm_root();
        let label = format!("stash@{{{index}}}");
        self.status = format!("Loading {label}\u{2026}");
        self.spawn_git_view(move || {
            let result = crate::git::show_stash(&root, index);
            Box::new(move |app: &mut App| match result {
                Ok(text) => match app.editor.open_text_buffer(Path::new(&label), &text) {
                    Ok(()) => {
                        app.focus_pane(Pane::Editor);
                        app.sync_open_file_poll_mtime();
                        app.status = format!("Opened {label}");
                    }
                    Err(e) => app.status = format!("Open stash failed: {e}"),
                },
                Err(e) => app.status = format!("Show stash failed: {e}"),
            })
        });
    }

    /// Open a commit's full patch (header, message, diffstat, diff) in a
    /// read-only scratch tab — the graph's click-through, tig's enter key.
    pub(super) fn open_commit_patch(&mut self, hash: &str, short_hash: &str) {
        let root = self.scm_root();
        let hash = hash.to_string();
        let short_hash = short_hash.to_string();
        self.status = format!("Loading commit {short_hash}\u{2026}");
        self.spawn_git_view(move || {
            let result = crate::git::show_commit(&root, &hash);
            Box::new(move |app: &mut App| match result {
                Ok(text) => {
                    let label = format!("commit {short_hash}");
                    match app.editor.open_text_buffer(Path::new(&label), &text) {
                        Ok(()) => {
                            app.focus_pane(Pane::Editor);
                            app.sync_open_file_poll_mtime();
                            app.status = format!("Opened commit {short_hash}");
                        }
                        Err(e) => app.status = format!("Open commit failed: {e}"),
                    }
                }
                Err(e) => app.status = format!("Show commit failed: {e}"),
            })
        });
    }

    /// Open the change at `entry_idx` in the editor pane: a side-by-side
    /// diff against HEAD for Modified / StagedModified entries (the
    /// VS Code Source Control click behaviour), or a plain file open
    /// for everything else where the working tree IS the only version
    /// (Untracked, StagedAdded) or HEAD doesn't carry the file
    /// (Deleted). Selection state on the panel updates so the user can
    /// see which entry the editor is bound to.
    pub fn open_source_control_entry(&mut self, entry_idx: usize) {
        use crate::git::ChangeKind;
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        self.source_control.selected_change = Some(entry_idx);
        // Porcelain paths are toplevel-relative (#139): join and HEAD-read
        // against the repo root, never the workspace root.
        let scm_root = self.scm_root();
        let abs = scm_root.join(&entry.path);
        let opened = match entry.kind {
            // A conflicted entry opens the three-way MERGE EDITOR (#253)
            // on the index stages — non-overlapping hunks pre-resolve,
            // and "Reopen as Text" reaches the old in-buffer marker flow.
            // If the stages are unreadable (e.g. binary), fall back to
            // opening the working file with its markers, parked on the
            // first conflict as before.
            ChangeKind::Conflicted => {
                if self.open_merge_editor_for(&abs) {
                    true
                } else {
                    match self.editor.open_pinned(&abs) {
                        Ok(()) => {
                            self.focus_pane(Pane::Editor);
                            let n = self.editor.conflicts().len();
                            if let Some(block) = self.editor.conflicts().first().copied() {
                                self.editor.clear_selection();
                                self.editor.cursor_row = block.ours_start;
                                self.editor.cursor_col = 0;
                                self.editor.scroll_col = 0;
                                self.editor.ensure_cursor_col_visible();
                            }
                            self.status = format!(
                                "{n} conflict{} — F7 jumps, Cmd+. accepts, \"Merge: Complete Merge\" stages",
                                if n == 1 { "" } else { "s" }
                            );
                            true
                        }
                        Err(e) => {
                            self.status = format!("Open failed: {e}");
                            false
                        }
                    }
                }
            }
            // A submodule has no HEAD blob to diff against (`git show
            // HEAD:lib` is "bad object"): show what moved in it, read-only,
            // the way a commit's patch opens (#1235).
            ChangeKind::Modified | ChangeKind::StagedModified
                if crate::git::is_submodule(&scm_root, &entry.path) =>
            {
                match crate::git::submodule_diff(&scm_root, &entry.path) {
                    Ok(text) => {
                        let label = format!("{} (submodule)", entry.path);
                        match self.editor.open_text_buffer(Path::new(&label), &text) {
                            Ok(()) => {
                                self.status = format!("Submodule {}", entry.path);
                                true
                            }
                            Err(e) => {
                                self.status = format!("Open failed: {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        self.status = format!("git diff on submodule failed: {e}");
                        false
                    }
                }
            }
            ChangeKind::Modified | ChangeKind::StagedModified => {
                // Both sides decoded the way the file's open tab decodes it,
                // so a windows-1252 or UTF-16 file diffs too (#1242).
                let enc = self.open_tab_encoding(&abs);
                let texts =
                    crate::git::read_bytes_at_head(&scm_root, &entry.path).and_then(|head| {
                        let working = std::fs::read(&abs).map_err(|e| e.to_string())?;
                        let decode = |b: &[u8]| crate::widgets::editor::decode_blob(b, enc);
                        decode(&head)
                            .zip(decode(&working))
                            .ok_or_else(|| format!("{} is not {}", entry.path, enc.name()))
                    });
                match texts {
                    Ok((head_text, working_text)) => {
                        let label = std::path::PathBuf::from(format!("{} (HEAD)", entry.path));
                        // The working side is the open tab's unsaved text
                        // when it has edits, in any split group.
                        let unsaved = self.unsaved_text_in_any_group(&abs);
                        match self.editor.open_head_diff_with_texts(
                            label,
                            &head_text,
                            unsaved.as_deref().unwrap_or(&working_text),
                            &abs,
                            true,
                            unsaved.is_some(),
                        ) {
                            Ok(()) => {
                                self.tag_open_diff(
                                    crate::widgets::diff::DiffSource::HeadVsWorking {
                                        root: scm_root.to_path_buf(),
                                        rel: entry.path.clone(),
                                    },
                                );
                                if let Some(diff) = self.editor.diff.as_mut() {
                                    diff.text_encoding = enc;
                                }
                                true
                            }
                            Err(e) => {
                                self.status = format!("Diff failed: {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        let why = self.git_error_line("show HEAD", &e);
                        self.status = format!("git show HEAD failed: {why}");
                        // Fall back to plain open so the user still sees
                        // something rather than nothing.
                        if abs.is_file() {
                            self.editor.open_pinned(&abs).is_ok()
                        } else {
                            false
                        }
                    }
                }
            }
            ChangeKind::Deleted | ChangeKind::StagedDeleted => {
                // No working-tree file to diff against — show the HEAD
                // blob as a one-sided unified diff so every removed
                // line is visible in red, matching `git diff` output
                // for a deletion.
                match crate::git::read_file_at_head(&scm_root, &entry.path) {
                    Ok(head_text) => {
                        let label = std::path::PathBuf::from(&entry.path);
                        match self.editor.open_deleted_diff_with_text(&label, &head_text) {
                            Ok(()) => true,
                            Err(e) => {
                                self.status = format!("Open deletion view failed: {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        let why = self.git_error_line("show HEAD", &e);
                        self.status = format!("git show HEAD failed: {why}");
                        false
                    }
                }
            }
            _ => {
                if abs.is_file() {
                    match self.editor.open_pinned(&abs) {
                        Ok(()) => true,
                        Err(e) => {
                            self.status = format!("Open failed: {e}");
                            false
                        }
                    }
                } else {
                    false
                }
            }
        };
        if opened {
            self.focus_pane(Pane::Editor);
            self.sync_open_file_poll_mtime();
        }
    }

    pub(super) fn initialize_repository(&mut self) {
        match std::process::Command::new("git")
            .arg("-C")
            .arg(&self.tree.root)
            .arg("init")
            .output()
        {
            Ok(out) if out.status.success() => {
                self.status = format!(
                    "Initialized empty Git repository in {}",
                    self.tree.root.display()
                );
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let why = self.git_error_line("init", &err);
                self.status = format!("git init failed: {why}");
            }
            Err(e) => {
                self.status = format!("git init failed: {e}");
            }
        }
    }

    pub fn stage_selected_source_control_entries(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let indices: Vec<usize> = if self.source_control.multi_selection.is_empty() {
            self.source_control
                .selected_change
                .iter()
                .copied()
                .collect()
        } else {
            self.source_control
                .multi_selection
                .iter()
                .copied()
                .collect()
        };
        if indices.is_empty() {
            self.status = String::from("No change selected to stage");
            return;
        }
        let paths: Vec<String> = indices
            .iter()
            .filter_map(|i| self.source_control.entries.get(*i))
            .filter(|e| e.kind.section() != crate::git::ChangeSection::Staged)
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            self.status = String::from("Selection is already staged");
            return;
        }
        match crate::git::stage_paths(&self.scm_root(), &paths) {
            Ok(()) => {
                self.status = if paths.len() == 1 {
                    format!("Staged {}", paths[0])
                } else {
                    format!("Staged {} files", paths.len())
                };
            }
            Err(err) => {
                let why = self.git_error_line("add", &err);
                self.status = format!("Stage failed: {why}");
                self.source_control.commit_feedback = Some(why);
                self.source_control.commit_feedback_is_error = true;
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Push the current branch without committing. Sibling of
    /// `commit_and_push_source_control`, called from the commit dropdown's
    /// "Push" item so users can ship already-committed work without
    /// having to type a no-op commit message first.
    pub fn push_source_control(&mut self) {
        let root = self.scm_root();
        self.spawn_git_net("push", move || {
            let r = crate::git::push_or_publish(&root);
            Box::new(move |app: &mut App| app.finish_push(r))
        });
    }

    /// Report a finished push on one line (#858): git's ref update, or its
    /// error, from [`crate::git::summary_line`]; the Git Output log keeps the
    /// `remote:` chatter and the rest.
    pub(super) fn finish_push(&mut self, result: Result<String, String>) {
        self.log_git("push", &result);
        let result = result
            .map(|out| crate::git::summary_line(&out))
            .map_err(|err| crate::git::error_line(&err));
        match result {
            Ok(summary) => {
                self.source_control.commit_feedback = Some(if summary.is_empty() {
                    "pushed".to_string()
                } else {
                    format!("pushed: {summary}")
                });
                self.source_control.commit_feedback_is_error = false;
                self.status = if summary.is_empty() {
                    String::from("Pushed")
                } else {
                    format!("Pushed: {summary}")
                };
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("push failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Push failed: {err}");
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
    }

    /// Pull the current branch from its upstream. Sibling of
    /// `push_source_control`, reached from the commit dropdown's "Pull".
    pub fn pull_source_control(&mut self) {
        let root = self.scm_root();
        self.spawn_git_net("pull", move || {
            let r = crate::git::pull_current_branch(&root);
            Box::new(move |app: &mut App| app.finish_pull(r))
        });
    }

    /// Report a finished pull. One that stopped on conflicts names them, and
    /// a failure names git's `error:` or `fatal:` line, on one line (#858);
    /// the Git Output log keeps git's whole text.
    pub(super) fn finish_pull(&mut self, result: Result<String, String>) {
        self.log_git("pull", &result);
        match result {
            Ok(summary) => {
                // git's result ("Fast-forward", "Already up to date."), not
                // the `From <url>` / `Updating a..b` lines before it (#858).
                let summary = crate::git::result_line(&summary);
                self.source_control.commit_feedback = Some(if summary.is_empty() {
                    "pulled".to_string()
                } else {
                    format!("pulled: {summary}")
                });
                self.source_control.commit_feedback_is_error = false;
                self.status = if summary.is_empty() {
                    String::from("Pulled")
                } else {
                    format!("Pulled: {summary}")
                };
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(crate::git::failure_line("pull", &err));
                self.source_control.commit_feedback_is_error = true;
                self.status = crate::git::failure_line("Pull", &err);
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Sync = pull then push, mirroring VS Code's sync button. A failed
    /// pull short-circuits (we never push on top of an unmerged tree); a
    /// successful pull followed by a failed push reports both halves. A
    /// branch with no upstream has nothing to pull, so it is published.
    pub fn sync_source_control(&mut self) {
        let root = self.scm_root();
        self.spawn_git_net("sync", move || {
            let pull = match crate::git::unpublished_branch(&root) {
                Some(_) => Ok(String::from("nothing, branch not published yet")),
                None => crate::git::pull_current_branch(&root),
            };
            // Never push on top of a failed pull.
            let push = pull.is_ok().then(|| crate::git::push_or_publish(&root));
            Box::new(move |app: &mut App| app.finish_sync(pull, push))
        });
    }

    /// Report a finished Sync on one line (#858): what the pull did and the
    /// push's ref update ("Synced: pulled: Fast-forward | pushed: a..b main
    /// -> main"), or why either half failed; the Git Output log keeps both
    /// of git's texts.
    pub(super) fn finish_sync(
        &mut self,
        pull: Result<String, String>,
        push: Option<Result<String, String>>,
    ) {
        self.log_git("pull", &pull);
        let pull_summary = match pull {
            Ok(s) => crate::git::result_line(&s),
            Err(err) => {
                // One line (#858): the conflicts the pull stopped on, or
                // git's error line; the Git Output log has the rest.
                let why = crate::git::error_line(&err);
                self.source_control.commit_feedback = Some(format!("sync: pull failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Sync failed on pull: {why}");
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
                return;
            }
        };
        let push = push.unwrap_or_else(|| Err(String::from("push did not run")));
        self.log_git("push", &push);
        match push {
            Ok(push_summary) => {
                let push_summary = crate::git::summary_line(&push_summary);
                let combined = [("pulled", pull_summary), ("pushed", push_summary)]
                    .into_iter()
                    .filter(|(_, summary)| !summary.is_empty())
                    .map(|(half, summary)| format!("{half}: {summary}"))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let (feedback, status) = if combined.is_empty() {
                    (String::from("synced"), String::from("Synced"))
                } else {
                    (format!("synced: {combined}"), format!("Synced: {combined}"))
                };
                self.source_control.commit_feedback = Some(feedback);
                self.source_control.commit_feedback_is_error = false;
                self.status = status;
            }
            Err(err) => {
                let why = crate::git::error_line(&err);
                self.source_control.commit_feedback = Some(format!("pull ok; push failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Sync: pull ok; push failed: {why}");
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Stash the working tree (`git stash push`). Reached from the commit
    /// dropdown's "Stash" item. Reported on one line (#858); the Git Output
    /// log keeps git's text.
    pub fn stash_source_control(&mut self) {
        if self.hold_for_unsaved(ScmWrite::Stash) {
            return;
        }
        self.ask_stash_message(crate::git::StashKind::Tracked);
    }

    /// Ask what to call the stash before making it (#1581), as VS Code's
    /// Git: Stash does, seeded with the commit box's first line. Without a
    /// name every stash on a commit read `WIP on <branch>: <commit>`, and
    /// the Apply / Pop / Drop pickers chose between identical rows.
    pub(super) fn ask_stash_message(&mut self, kind: crate::git::StashKind) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let title = match kind {
            crate::git::StashKind::Tracked => "Stash",
            crate::git::StashKind::IncludeUntracked => "Stash (Include Untracked)",
            crate::git::StashKind::Staged => "Stash Staged",
        };
        let seed = self
            .source_control
            .message
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::StashMessage { kind },
                title,
                "Message (optional)",
            )
            .allowing_blank()
            .with_hint("Enter names the stash \u{b7} left blank, git names it WIP on <branch>")
            .with_value(seed),
        );
    }

    /// Make the stash the prompt asked about, named `message` when given.
    pub(super) fn perform_stash(&mut self, kind: crate::git::StashKind, message: Option<&str>) {
        match kind {
            crate::git::StashKind::Tracked => self.stash_tracked(message),
            crate::git::StashKind::IncludeUntracked => {
                let r = crate::git::stash_push_untracked(&self.scm_root(), message);
                self.run_scm_op(
                    "stash push -u",
                    r,
                    "Stashed (incl. untracked)",
                    "Stash (incl. untracked)",
                );
            }
            crate::git::StashKind::Staged => {
                let r = crate::git::stash_push_staged(&self.scm_root(), message);
                self.run_scm_op("stash push --staged", r, "Stashed staged", "Stash staged");
            }
        }
    }

    pub(super) fn stash_tracked(&mut self, message: Option<&str>) {
        let stashed = crate::git::stash_push(&self.scm_root(), message);
        self.log_git("stash push", &stashed);
        match stashed {
            Ok(summary) => {
                let summary = crate::git::result_line(&summary);
                let (feedback, status) = if summary.is_empty() {
                    (String::from("stashed"), String::from("Stashed"))
                } else {
                    (format!("stashed: {summary}"), format!("Stashed: {summary}"))
                };
                self.source_control.commit_feedback = Some(feedback);
                self.source_control.commit_feedback_is_error = false;
                self.status = status;
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(crate::git::failure_line("stash", &err));
                self.source_control.commit_feedback_is_error = true;
                self.status = crate::git::failure_line("Stash", &err);
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Restore the most recent stash (`git stash pop`). A pop that stops on
    /// conflicts names them on one line (#858), so the user can resolve
    /// them; the Git Output log keeps git's whole text.
    pub fn stash_pop_source_control(&mut self) {
        let popped = crate::git::stash_pop(&self.scm_root());
        self.log_git("stash pop", &popped);
        match popped {
            Ok(summary) => {
                // The stash it dropped, not the `git status` dump before it.
                let summary = crate::git::result_line(&summary);
                let (feedback, status) = if summary.is_empty() {
                    (String::from("popped"), String::from("Stash popped"))
                } else {
                    (
                        format!("popped: {summary}"),
                        format!("Stash popped: {summary}"),
                    )
                };
                self.source_control.commit_feedback = Some(feedback);
                self.source_control.commit_feedback_is_error = false;
                self.status = status;
            }
            Err(err) => {
                self.source_control.commit_feedback =
                    Some(crate::git::failure_line("stash pop", &err));
                self.source_control.commit_feedback_is_error = true;
                self.status = crate::git::failure_line("Stash pop", &err);
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Unstage a single entry by index (the inline "−" icon on a staged
    /// row). Mirror of `stage_source_control_entry`.
    pub fn unstage_source_control_entry(&mut self, entry_idx: usize) {
        if self.repo_git_busy() {
            return;
        }
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        match crate::git::unstage_path(&self.scm_root(), &entry.path) {
            Ok(()) => {
                self.status = format!("Unstaged {}", entry.path);
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                let why = self.git_error_line("reset -q HEAD --", &err);
                self.status = format!("Unstage failed: {why}");
                self.source_control.commit_feedback = Some(why);
                self.source_control.commit_feedback_is_error = true;
            }
        }
    }
}
