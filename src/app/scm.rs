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

    /// Re-read git status for each repository whose git state moved outside
    /// croft (#2577): a commit, checkout, staging, fetch or push run in
    /// another terminal changes only git's own folder, which the file
    /// watcher skips. Called on the watcher's poll. True when any asked.
    pub(super) fn refresh_git_status_if_git_moved(&mut self) -> bool {
        let want_changes = self.sidebar_view == SidebarView::SourceControl;
        let active = self.active_scm_root.clone();
        let mut asked = false;
        let mut active_moved = false;
        if self.git.refresh_if_git_moved(want_changes) {
            asked = true;
            active_moved |= self.roots.primary() == active.as_path();
        }
        for (root, w) in &mut self.git_extra {
            if w.refresh_if_git_moved(want_changes) {
                asked = true;
                active_moved |= *root == active;
            }
        }
        // COMMITS labels each commit with its refs: a checkout to a branch
        // at the same commit, a fetch, a push or a new tag relabels it
        // without moving HEAD, which is all the status refresh refetches
        // the graph for.
        if active_moved {
            self.refresh_commit_graph();
        }
        asked
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
        // Ship any refresh the debounce window coalesced once the gap clears,
        // so the trailing edge of an edit burst lands without another FS event.
        let mut changed = false;
        // Worked out off this thread (#1973): the answer lands a tick or more
        // later, and a rebind then owes the Testing view a redraw.
        let mut test_rebound = false;
        if let Some(test_root) = self.poll_followed_test_root(follow)
            && test_root != self.active_test_root
        {
            self.rebind_test_root(test_root);
            test_rebound = true;
        }
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
            // Its worker crashed again soon after being started again, so
            // it stopped rather than loop (#2589): say what is frozen and
            // how to retry.
            if w.take_stopped() {
                self.status = String::from(
                    "Git status stopped after its worker crashed twice: Source Control: Refresh starts it again",
                );
                claim_background_panic(git_worker::THREAD);
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
            self.refresh_activity_badges();
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
        changed || diffs_refreshed || test_rebound
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
    /// pure lane layout both run as the `commit-graph` job, and the finished
    /// rows come back tagged with the root they describe. The job in flight
    /// collapses a burst of triggers into one shell-out plus one
    /// queued refetch: a trigger during flight is answering a question the
    /// in-flight fetch predates (a Make Root, a newer HEAD), so it must
    /// re-fire when the reply drains rather than be dropped — a root change
    /// racing the fetch otherwise left the panel on "Loading" until HEAD
    /// next moved.
    pub(super) fn refresh_commit_graph(&mut self) {
        if self.graph_fetch.is_some() {
            self.graph_refetch_queued = true;
            return;
        }
        // The COMMITS section follows the active repo (#149).
        let root = self.active_scm_root.clone();
        let revspec = self.graph_revspec.clone();
        let job = crate::jobs::Job::spawn("commit-graph", crate::jobs::Start::Now, move || {
            let commits = match revspec.as_deref() {
                Some(spec) => crate::git::branch_history_range(&root, spec, 400),
                None => crate::git::commit_graph(&root, 400),
            };
            let rows = crate::widgets::commit_graph::layout_graph(commits);
            (root, revspec, rows)
        });
        match job {
            Ok(job) => self.graph_fetch = Some(job),
            Err(e) => self.status = format!("Could not start the commit graph: {e}"),
        }
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

impl App {
    // --- Source Control "⋯" menu plumbing --------------------------------

    /// Append a git command + its outcome to the rolling output log, which
    /// the "Show Git Output" action surfaces read-only. Capped so a long
    /// session can't grow it without bound.
    pub(super) fn log_git(&mut self, label: &str, outcome: &Result<String, String>) {
        let body = match outcome {
            Ok(s) if s.is_empty() => String::from("(ok)"),
            Ok(s) => s.clone(),
            Err(e) => format!("error: {e}"),
        };
        self.git_output_log.push(format!("$ git {label}\n{body}"));
        const CAP: usize = 200;
        if self.git_output_log.len() > CAP {
            let excess = self.git_output_log.len() - CAP;
            self.git_output_log.drain(0..excess);
        }
    }

    /// Git's error `err` on the one line the status bar and the Source
    /// Control panel show ([`crate::git::error_line`]), its whole text sent
    /// to the Git Output log under `label` (#858).
    pub(super) fn git_error_line(&mut self, label: &str, err: &str) -> String {
        self.log_git(label, &Err(err.to_string()));
        crate::git::error_line(err)
    }

    /// The repository's operation in progress (#1356), read fresh, or
    /// `None` with a status saying there is none.
    pub(super) fn git_operation_or_say(&mut self) -> Option<crate::git::RepoOp> {
        let op = crate::git::repo_operation(&self.scm_root());
        if op.is_none() {
            self.status =
                String::from("No merge, rebase, cherry-pick, revert or bisect in progress");
        }
        op
    }

    /// Abort the merge, rebase, cherry-pick or revert in progress, or reset
    /// a bisect (#1356; VS Code's Abort Merge / Abort Rebase).
    pub(super) fn abort_git_operation(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let Some(op) = self.git_operation_or_say() else {
            return;
        };
        let r = crate::git::abort_operation(&self.scm_root(), op);
        let done = match op {
            crate::git::RepoOp::Bisect => String::from("Bisect reset"),
            _ => format!("Aborted the {}", op.command()),
        };
        self.run_scm_op(op.command(), r, &done, &format!("Git {}", op.command()));
    }

    /// Continue the rebase, cherry-pick or revert in progress once its
    /// conflicts are resolved and staged (#1356; VS Code's Continue Rebase).
    pub(super) fn continue_git_operation(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let Some(op) = self.git_operation_or_say() else {
            return;
        };
        let r = crate::git::continue_operation(&self.scm_root(), op);
        self.report_git_operation_step(op, r, "Continued");
    }

    /// Skip the commit the rebase, cherry-pick or revert stopped on
    /// (#1356).
    pub(super) fn skip_git_operation(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let Some(op) = self.git_operation_or_say() else {
            return;
        };
        let r = crate::git::skip_operation(&self.scm_root(), op);
        self.report_git_operation_step(op, r, "Skipped a commit of");
    }

    /// Mark the commit a bisect is on good or bad (#1356).
    pub(super) fn bisect_mark_source_control(&mut self, good: bool) {
        if self.repo_git_busy() {
            return;
        }
        let r = crate::git::bisect_mark(&self.scm_root(), good);
        self.run_scm_op(
            "bisect",
            r,
            if good { "Marked good" } else { "Marked bad" },
            "Bisect",
        );
    }

    /// Report a Continue or Skip, saying where the operation stands now:
    /// finished, or stopped again (the next conflict, with its step).
    pub(super) fn report_git_operation_step(
        &mut self,
        op: crate::git::RepoOp,
        r: Result<String, String>,
        verb: &str,
    ) {
        let now = crate::git::repo_operation(&self.scm_root());
        let done = match now {
            None => format!("{verb} the {}, which is done", op.command()),
            Some(next) => format!("{verb} the {}, now {}", op.command(), next.label()),
        };
        self.run_scm_op(op.command(), r, &done, &format!("Git {}", op.command()));
    }

    /// After a commit made from Source Control mid-rebase (#1356, the #989
    /// path): carry on with the rebase, as VS Code's Commit button does
    /// there, instead of leaving HEAD detached with the rest unapplied.
    pub(super) fn continue_rebase_after_commit(&mut self) -> bool {
        let root = self.scm_root();
        let Some(op @ crate::git::RepoOp::Rebase { .. }) = crate::git::repo_operation(&root) else {
            return true;
        };
        let r = crate::git::continue_operation(&root, op);
        let continued = r.is_ok();
        self.report_git_operation_step(op, r, "Committed and continued");
        continued
    }

    /// Run an immediate git operation: log it, surface its summary or error
    /// in the commit-feedback line, and refresh the panel. The single path
    /// every "⋯"-menu leaf that acts now (vs. opening a modal) flows through.
    /// `ok_prefix` heads a success ("Merged: …"); `noun` names the operation
    /// when it fails ("Merge failed: …", "Merge: 1 conflict (pricing.py)"),
    /// on one line from [`crate::git::failure_line`] (#858).
    pub(super) fn run_scm_op(
        &mut self,
        label: &str,
        outcome: Result<String, String>,
        ok_prefix: &str,
        noun: &str,
    ) {
        self.log_git(label, &outcome);
        match outcome {
            Ok(summary) => {
                // git's result line, past a fetch's `From <url>`, a push's
                // `To <url>` and the like (#858).
                let summary = crate::git::result_line(&summary);
                let line = if summary.is_empty() {
                    ok_prefix.to_string()
                } else {
                    format!("{ok_prefix}: {summary}")
                };
                self.source_control.commit_feedback = Some(line.clone());
                self.source_control.commit_feedback_is_error = false;
                self.status = line;
            }
            Err(err) => {
                let line = crate::git::failure_line(noun, &err);
                self.source_control.commit_feedback = Some(line.clone());
                self.source_control.commit_feedback_is_error = true;
                self.status = line;
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    /// Run a network git operation (push, pull, fetch, clone...) on a
    /// worker thread. Run inline, a slow remote or a large clone froze the
    /// whole UI until git returned. `job` runs off the UI thread and returns
    /// the half that updates the app, which `drain_git_net` applies. One at
    /// a time: two pushes racing each other help nobody.
    pub(super) fn spawn_git_net(
        &mut self,
        what: &str,
        job: impl FnOnce() -> GitNetDone + Send + 'static,
    ) {
        if self.git_net_busy() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.git_net_job = Some((what.to_string(), rx));
        self.status = format!("Git: {what}\u{2026}");
    }

    /// Run a read-only git view off the UI thread; its finisher is applied by
    /// [`Self::drain_git_view`]. A newer request supersedes a pending one,
    /// whose result is then dropped.
    pub(super) fn spawn_git_view(&mut self, job: impl FnOnce() -> GitNetDone + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.git_view_job = Some(rx);
    }

    /// Apply a finished git view. True when one landed.
    pub(super) fn drain_git_view(&mut self) -> bool {
        let Some(rx) = &self.git_view_job else {
            return false;
        };
        match rx.try_recv() {
            Ok(done) => {
                self.git_view_job = None;
                done(self);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.git_view_job = None;
                false
            }
        }
    }

    /// Whether a network operation on THIS repository is still running,
    /// saying so: what a commit, stage or branch switch must not overlap
    /// (they fight over `index.lock`, and a pull would land on the switched
    /// branch). A clone writes into another directory and blocks nothing here.
    pub(super) fn repo_git_busy(&mut self) -> bool {
        match &self.git_net_job {
            Some((running, _)) if running != "clone" => {
                self.status = format!("Git: {running} is still running");
                true
            }
            _ => false,
        }
    }

    /// Whether a network git operation is still running, saying so.
    pub(super) fn git_net_busy(&mut self) -> bool {
        let Some((running, _)) = &self.git_net_job else {
            return false;
        };
        self.status = format!("Git: {running} is still running");
        true
    }

    /// [`spawn_git_net`] for an operation whose result is reported by
    /// [`run_scm_op`].
    pub(super) fn spawn_scm_op(
        &mut self,
        label: &'static str,
        ok_prefix: &'static str,
        noun: &'static str,
        op: impl FnOnce(&Path) -> Result<String, String> + Send + 'static,
    ) {
        let root = self.scm_root();
        self.spawn_git_net(label, move || {
            let r = op(&root);
            Box::new(move |app: &mut App| app.run_scm_op(label, r, ok_prefix, noun))
        });
    }

    pub(super) fn finish_clone(&mut self, result: Result<PathBuf, String>, origin: &Path) {
        match result {
            Ok(dest) => {
                self.log_git("clone", &Ok(format!("into {}", dest.display())));
                self.status = format!("Cloned into {}", self.status_path(&dest));
                // Opened only if the user is still where the clone started:
                // one who has moved to another folder meanwhile is not
                // pulled out of it when the clone lands.
                if self.tree.root == origin {
                    self.change_workspace_root(dest);
                }
            }
            Err(err) => {
                let why = self.git_error_line("clone", &err);
                self.source_control.commit_feedback = Some(format!("clone failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Clone failed: {why}");
            }
        }
    }

    /// Apply a finished network git operation. True when one landed.
    pub(super) fn drain_git_net(&mut self) -> bool {
        let Some((what, rx)) = &self.git_net_job else {
            return false;
        };
        match rx.try_recv() {
            Ok(done) => {
                self.git_net_job = None;
                done(self);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.status = format!("Git: {what} ended without a result");
                self.git_net_job = None;
                true
            }
        }
    }

    pub fn stage_all_source_control(&mut self) {
        let r = crate::git::stage_all(&self.scm_root());
        self.run_scm_op("add -A", r, "Staged all", "Stage all");
    }

    pub fn unstage_all_source_control(&mut self) {
        let r = crate::git::unstage_all(&self.scm_root());
        self.run_scm_op("reset HEAD", r, "Unstaged all", "Unstage all");
    }

    /// Arm the Discard All confirmation. Destructive (reverts every tracked
    /// edit to HEAD), so it never acts until `confirm_pending_discard_all`.
    pub fn request_discard_all_source_control(&mut self) {
        self.pending_discard_all = true;
    }

    /// Drop the stash the Drop Stash confirm was asked about (#1128).
    pub(super) fn confirm_pending_stash_drop(&mut self) {
        let Some(pending) = self.pending_stash_drop.take() else {
            return;
        };
        let r = crate::git::stash_drop_oid(&pending.root, &pending.oid);
        self.run_scm_op("stash drop", r, "Dropped stash", "Drop stash");
    }

    pub fn cancel_pending_discard_all(&mut self) {
        self.pending_discard_all = false;
    }

    pub fn confirm_pending_discard_all(&mut self) {
        if !self.pending_discard_all {
            return;
        }
        self.pending_discard_all = false;
        let r = crate::git::discard_all_tracked(&self.scm_root());
        self.run_scm_op(
            "checkout -- .",
            r,
            "Discarded all changes",
            "Discard all changes",
        );
    }

    /// `git stash push -u` from the commit dropdown.
    pub(super) fn stash_include_untracked_source_control(&mut self) {
        if self.hold_for_unsaved(ScmWrite::StashIncludeUntracked) {
            return;
        }
        self.ask_stash_message(crate::git::StashKind::IncludeUntracked);
    }

    /// `git stash push --staged` from the commit dropdown.
    pub(super) fn stash_staged_source_control(&mut self) {
        if self.hold_for_unsaved(ScmWrite::StashStaged) {
            return;
        }
        self.ask_stash_message(crate::git::StashKind::Staged);
    }

    /// The files of tabs with unsaved edits under the repository, each
    /// once, in tab order. Untitled tabs and files outside the repository
    /// are not the commit's business (#1438). A spreadsheet cell still being
    /// typed into counts: its value is on screen but not yet in the buffer.
    pub(super) fn unsaved_repo_files(&self) -> Vec<PathBuf> {
        let root = self.scm_root();
        let mut files: Vec<PathBuf> = Vec::new();
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        let unsaved = |e: &&crate::widgets::editor::Editor| {
            // A scratch tab (#951) is no file of the repository's, though
            // a Git Output tab's label sits under its root.
            (e.dirty && !e.scratch)
                || e.sheet
                    .as_ref()
                    .is_some_and(|s| s.editable() && s.editing.is_some())
        };
        for ed in groups.flat_map(|g| g.editors.iter()).filter(unsaved) {
            if let Some(p) = ed.path.as_ref()
                && p.starts_with(&root)
                && !files.contains(p)
            {
                files.push(p.clone());
            }
        }
        files
    }

    /// Whether `op` must wait for the unsaved-changes prompt (#1438). A
    /// commit or stash records the files on disk, so an edit still only in
    /// a tab would be left out with nothing said; VS Code asks first
    /// (`git.promptToSaveFilesBeforeCommit` / `...BeforeStash`). True when
    /// the prompt opened and the caller must stop.
    pub(super) fn hold_for_unsaved(&mut self, op: ScmWrite) -> bool {
        if std::mem::take(&mut self.scm_unsaved_cleared) {
            return false;
        }
        let files = self.unsaved_repo_files();
        if files.is_empty() {
            return false;
        }
        let root = self.scm_root();
        self.pending_unsaved_scm = Some(PendingUnsavedScm { op, root, files });
        true
    }

    /// Whether Source Control now points at a repository other than `root`,
    /// the one `op` was asked for (focus moved to another root's tab while
    /// the prompt was open). The write is then dropped rather than run
    /// against the wrong repository, and the status says so.
    pub(super) fn scm_root_moved(&mut self, op: ScmWrite, root: &Path) -> bool {
        if self.scm_root() == root {
            return false;
        }
        let msg = format!(
            "{} cancelled: Source Control moved to another repository",
            op.verb()
        );
        self.status = msg.clone();
        self.source_control.commit_feedback = Some(msg);
        self.source_control.commit_feedback_is_error = true;
        true
    }

    /// Run `op` past the unsaved-changes check, which already had its say.
    pub(super) fn run_scm_write(&mut self, op: ScmWrite) {
        self.scm_unsaved_cleared = true;
        match op {
            ScmWrite::Commit => self.commit_source_control(),
            ScmWrite::CommitStaged => self.commit_staged_source_control(),
            ScmWrite::CommitAll => self.commit_all_source_control(),
            ScmWrite::Amend => self.commit_amend_source_control(),
            ScmWrite::CommitAndSync => self.commit_and_sync_source_control(),
            ScmWrite::CommitAndPush => self.commit_and_push_source_control(),
            ScmWrite::Stash => self.stash_source_control(),
            ScmWrite::StashIncludeUntracked => self.stash_include_untracked_source_control(),
            ScmWrite::StashStaged => self.stash_staged_source_control(),
            ScmWrite::StashAndSwitch => self.stash_and_switch_picked(false),
            ScmWrite::MigrateAndSwitch => self.stash_and_switch_picked(true),
        }
        // A write that stopped before its check (busy git, empty message)
        // must not carry the clearance to the next request.
        self.scm_unsaved_cleared = false;
    }

    /// "Save All & Commit" (or Stash): save every dirty tab, then run the
    /// write only when nothing under the repository is left unsaved, so the
    /// commit never records half of the edits.
    pub fn save_all_then_pending_scm(&mut self) {
        let Some(pending) = self.pending_unsaved_scm.take() else {
            return;
        };
        if self.scm_root_moved(pending.op, &pending.root) {
            return;
        }
        // A cell still being typed into is part of the save, as Cmd+S
        // treats it (#1140): commit it so Save All writes it.
        for group in
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut())
        {
            for ed in &mut group.editors {
                if let Some(view) = ed.sheet.as_mut()
                    && view.editable()
                    && view.commit_edit().is_some()
                {
                    ed.mark_dirty();
                }
            }
        }
        self.save_all();
        if self.save_after_format.is_some() {
            // The active tab is written when its formatter replies.
            self.scm_after_format_save = Some((pending.op, pending.root));
            return;
        }
        self.finish_scm_after_save(pending.op, &pending.root);
    }

    /// Run `op` once Save All is done, or say which files kept it from
    /// running.
    pub(super) fn finish_scm_after_save(&mut self, op: ScmWrite, root: &Path) {
        if self.scm_root_moved(op, root) {
            return;
        }
        let left = self.unsaved_repo_files();
        if left.is_empty() {
            self.run_scm_write(op);
            return;
        }
        let names: Vec<String> = left
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        self.status = format!(
            "{} cancelled: {} still unsaved - open it and press Cmd+S",
            op.verb(),
            names.join(", ")
        );
        self.source_control.commit_feedback = Some(format!(
            "{} cancelled: {} not saved",
            op.verb(),
            names.join(", ")
        ));
        self.source_control.commit_feedback_is_error = true;
    }

    /// "Commit Anyway" (or Stash): run the write on the files as they are on
    /// disk, as croft did before it asked.
    pub fn continue_pending_scm_without_saving(&mut self) {
        if let Some(pending) = self.pending_unsaved_scm.take()
            && !self.scm_root_moved(pending.op, &pending.root)
        {
            self.run_scm_write(pending.op);
        }
    }

    pub fn cancel_pending_unsaved_scm(&mut self) {
        self.pending_unsaved_scm = None;
    }

    /// The commit message currently in the input box, trimmed, or an error
    /// surfaced when it is empty (shared by the Commit submenu variants).
    pub(super) fn require_commit_message(&mut self) -> Option<String> {
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback = Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return None;
        }
        Some(message)
    }

    pub(super) fn commit_staged_source_control(&mut self) {
        let Some(message) = self.require_commit_message() else {
            return;
        };
        if self.hold_for_unsaved(ScmWrite::CommitStaged) {
            return;
        }
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || crate::git::commit_staged(&root, &committed),
            |app, r| {
                let committed = r.is_ok();
                app.run_scm_op("commit -m", r, "Committed staged", "Commit staged");
                if committed {
                    app.continue_rebase_after_commit();
                }
            },
        );
    }

    pub(super) fn commit_all_source_control(&mut self) {
        let Some(message) = self.require_commit_message() else {
            return;
        };
        // Before `add -A`, which would mark a conflict resolved with its
        // markers still in it (#989).
        if let Err(err) = crate::git::refuse_unmerged(&self.scm_root()) {
            self.run_scm_op("add -A", Err(err), "Stage all", "Stage all");
            return;
        }

        if self.hold_for_unsaved(ScmWrite::CommitAll) {
            return;
        }
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || {
                crate::git::stage_all(&root).map_err(|err| format!("stage all failed: {err}"))?;
                crate::git::commit_staged(&root, &committed)
            },
            |app, r| {
                let committed = r.is_ok();
                app.run_scm_op("commit (all)", r, "Committed all", "Commit all");
                if committed {
                    app.continue_rebase_after_commit();
                }
            },
        );
    }

    pub(super) fn commit_amend_source_control(&mut self) {
        // Amend keeps the prior message when the box is empty, otherwise
        // rewrites it — matching VS Code's amend.
        if self.hold_for_unsaved(ScmWrite::Amend) {
            return;
        }
        let message = self.source_control.message.trim().to_string();
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || {
                if committed.is_empty() {
                    crate::git::commit_amend_no_edit(&root)
                } else {
                    crate::git::commit_amend(&root, &committed)
                }
            },
            |app, r| app.run_scm_op("commit --amend", r, "Amended commit", "Amend commit"),
        );
    }

    /// Undo Last Commit (#1350): the last commit goes back to Staged and
    /// its message back into an empty box, so a reword or a re-pick of the
    /// files is one step away. A commit already on the upstream is undone
    /// only on a second ask, after a warning that it means a force push.
    pub(super) fn undo_last_commit_source_control(&mut self) {
        let root = self.scm_root();
        let head = crate::git::query(&root).head_oid;
        if head.is_some()
            && crate::git::head_is_on_upstream(&root)
            && self.undo_pushed_commit_armed != head
        {
            self.undo_pushed_commit_armed = head;
            let warning = String::from(
                "The last commit is already pushed: undoing it means a force push later. Undo Last Commit again to undo it anyway",
            );
            self.source_control.commit_feedback = Some(warning.clone());
            self.source_control.commit_feedback_is_error = true;
            self.status = warning;
            return;
        }
        self.undo_pushed_commit_armed = None;
        let outcome = crate::git::undo_last_commit(&root);
        if let Ok(message) = &outcome
            && self.source_control.message.trim().is_empty()
        {
            self.source_control.clear_message();
            self.source_control.insert_str(message.trim());
        }
        let summary = outcome.map(|m| m.lines().next().unwrap_or("").trim().to_string());
        self.run_scm_op(
            "reset --soft HEAD~1",
            summary,
            "Undid last commit",
            "Undo last commit",
        );
    }

    pub(super) fn commit_and_sync_source_control(&mut self) {
        if self.git_net_busy() {
            return;
        }
        let Some(message) = self.require_commit_message() else {
            return;
        };
        if self.hold_for_unsaved(ScmWrite::CommitAndSync) {
            return;
        }
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || crate::git::commit(&root, &committed),
            |app, commit| {
                app.log_git("commit", &commit);
                if let Err(err) = commit {
                    app.report_commit_failure(&err);
                    return;
                }
                // Mid-rebase, sync only once the rebase is back on its branch.
                if !app.continue_rebase_after_commit() {
                    return;
                }
                app.sync_source_control();
            },
        );
    }

    pub(super) fn publish_branch_source_control(&mut self) {
        let Some(branch) = self.source_control.status.branch.clone() else {
            self.source_control.commit_feedback =
                Some(String::from("Detached HEAD has no branch to publish"));
            self.source_control.commit_feedback_is_error = true;
            return;
        };
        self.spawn_scm_op("push -u", "Published", "Publish branch", move |root| {
            crate::git::publish_branch(root, &branch)
        });
    }

    /// Open the read-only "Git Output" log in an editor tab (VS Code's Git
    /// output channel). Empty until croft has run an operation this session.
    pub(super) fn show_git_output(&mut self) {
        let body = if self.git_output_log.is_empty() {
            String::from("(no git operations run yet this session)")
        } else {
            self.git_output_log.join("\n\n")
        };
        let label = self.scratch_buffer_path("Git Output");
        if let Err(err) = self.editor.open_text_buffer(&label, &body) {
            self.status = format!("Could not open Git Output: {err}");
            return;
        }
        self.focus_pane(Pane::Editor);
    }

    /// Open the branch picker with an explicit purpose (Checkout by default,
    /// or one of the Branch-submenu verbs).
    pub(super) fn open_branch_picker_for(&mut self, purpose: BranchPurpose) {
        self.branch_purpose = purpose;
        self.open_branch_picker();
    }

    /// Route every "⋯"-menu leaf to its handler. Immediate ops run now;
    /// ops needing a value or a choice open a modal/picker.
    pub fn dispatch_scm_action(&mut self, action: crate::widgets::scm_menu::ScmAction) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        use crate::widgets::list_picker::ListPurpose;
        use crate::widgets::scm_menu::ScmAction;
        self.scm_menu.close();
        // Nothing else touches the repository while a push, pull or fetch
        // runs in the background: a commit or branch switch under it fights
        // over `index.lock`, and a pull would land on the switched branch.
        if !matches!(action, ScmAction::ShowGitOutput) && self.repo_git_busy() {
            return;
        }
        match action {
            ScmAction::Pull => self.pull_source_control(),
            ScmAction::Push => self.push_source_control(),
            ScmAction::Clone => self.open_input_prompt(InputPrompt::new(
                InputPurpose::CloneUrl,
                "Clone Repository",
                "Repository URL (https://… or git@…)",
            )),
            ScmAction::CheckoutTo => self.open_branch_picker_for(BranchPurpose::Checkout),
            ScmAction::Fetch => {
                self.spawn_scm_op(
                    "fetch --all --prune",
                    "Fetched",
                    "Fetch",
                    crate::git::fetch_all,
                );
            }
            ScmAction::ShowGitOutput => self.show_git_output(),
            ScmAction::ContinueOperation => self.continue_git_operation(),
            ScmAction::SkipOperation => self.skip_git_operation(),
            ScmAction::AbortOperation => self.abort_git_operation(),
            ScmAction::BisectGood => self.bisect_mark_source_control(true),
            ScmAction::BisectBad => self.bisect_mark_source_control(false),
            ScmAction::Commit => self.commit_source_control(),
            ScmAction::CommitStaged => self.commit_staged_source_control(),
            ScmAction::CommitAll => self.commit_all_source_control(),
            ScmAction::CommitAmend => self.commit_amend_source_control(),
            ScmAction::UndoLastCommit => self.undo_last_commit_source_control(),
            ScmAction::CommitAndPush => self.commit_and_push_source_control(),
            ScmAction::CommitAndSync => self.commit_and_sync_source_control(),
            ScmAction::StageAll => self.stage_all_source_control(),
            ScmAction::UnstageAll => self.unstage_all_source_control(),
            ScmAction::DiscardAll => self.request_discard_all_source_control(),
            ScmAction::Sync => self.sync_source_control(),
            ScmAction::PullRebase => {
                self.spawn_scm_op(
                    "pull --rebase",
                    "Pulled (rebase)",
                    "Pull (rebase)",
                    crate::git::pull_rebase,
                );
            }
            ScmAction::PushTo => self.open_push_to_remote_picker(),
            ScmAction::PushForce => {
                self.spawn_scm_op(
                    "push --force-with-lease",
                    "Force-pushed",
                    "Force push",
                    crate::git::push_force,
                );
            }
            ScmAction::PublishBranch => self.publish_branch_source_control(),
            ScmAction::CreateBranch => self.open_branch_picker_for(BranchPurpose::Checkout),
            ScmAction::CreateBranchFrom => self.open_branch_picker_for(BranchPurpose::CreateFrom),
            ScmAction::RenameBranch => {
                let seed = self
                    .source_control
                    .status
                    .branch
                    .clone()
                    .unwrap_or_default();
                self.open_input_prompt(
                    InputPrompt::new(
                        InputPurpose::RenameBranch,
                        "Rename Branch",
                        "New branch name",
                    )
                    .with_value(seed),
                );
            }
            ScmAction::DeleteBranch => self.open_branch_picker_for(BranchPurpose::Delete),
            ScmAction::Merge => self.open_branch_picker_for(BranchPurpose::Merge),
            ScmAction::Rebase => self.open_branch_picker_for(BranchPurpose::Rebase),
            ScmAction::AddRemote => self.open_input_prompt(InputPrompt::new(
                InputPurpose::AddRemoteName,
                "Add Remote",
                "Remote name (e.g. origin)",
            )),
            ScmAction::RemoveRemote => self.open_remote_picker(ListPurpose::RemoveRemote),
            ScmAction::Stash => self.stash_source_control(),
            ScmAction::StashIncludeUntracked => self.stash_include_untracked_source_control(),
            ScmAction::StashStaged => self.stash_staged_source_control(),
            ScmAction::ApplyStash => self.open_stash_picker(ListPurpose::StashApply),
            ScmAction::PopStashLatest => self.stash_pop_source_control(),
            ScmAction::PopStashPick => self.open_stash_picker(ListPurpose::StashPop),
            ScmAction::DropStash => self.open_stash_picker(ListPurpose::StashDrop),
            ScmAction::ViewStash => self.open_stash_picker(ListPurpose::StashView),
            ScmAction::CreateTag => self.open_input_prompt(InputPrompt::new(
                InputPurpose::CreateTag,
                "Create Tag",
                "Tag name (e.g. v1.0.0)",
            )),
            ScmAction::DeleteTag => self.open_tag_picker(),
            ScmAction::PushTags => self.push_tags_source_control(),
            ScmAction::DeleteRemoteTag => self.open_remote_tag_picker(),
        }
    }

    /// Source Control > Tags > Push Tags (#1582): every local tag to the
    /// remote a new branch would be published to.
    pub(super) fn push_tags_source_control(&mut self) {
        let remote = match crate::git::tag_remote(&self.scm_root()) {
            Ok(remote) => remote,
            Err(err) => {
                self.status = format!("Push tags: {err}");
                return;
            }
        };
        self.spawn_scm_op("push --tags", "Pushed tags", "Push tags", move |root| {
            crate::git::push_tags(root, &remote)
        });
    }

    /// Source Control > Tags > Delete Remote Tag (#1582): list the push
    /// remote's tags off the UI thread, then pick one to delete there.
    pub(super) fn open_remote_tag_picker(&mut self) {
        let root = self.scm_root();
        let remote = match crate::git::tag_remote(&root) {
            Ok(remote) => remote,
            Err(err) => {
                self.status = format!("Delete remote tag: {err}");
                return;
            }
        };
        self.spawn_git_net("ls-remote --tags", move || {
            let tags = crate::git::list_remote_tags(&root, &remote);
            Box::new(move |app: &mut App| app.show_remote_tags(&root, &remote, tags))
        });
    }

    /// Whether Source Control now points at a repository other than
    /// `root`, the one whose remote tags were listed (#1582). The deletion
    /// is then dropped rather than sent to another repository's remote of
    /// the same name, and the status says so.
    pub(super) fn remote_tag_root_moved(&mut self, root: &Path) -> bool {
        if self.scm_root() == root {
            return false;
        }
        self.status =
            String::from("Delete remote tag cancelled: Source Control moved to another repository");
        true
    }

    /// The picker over `remote`'s tags in `root`, once listed (#1582).
    pub(super) fn show_remote_tags(
        &mut self,
        root: &Path,
        remote: &str,
        tags: Result<Vec<String>, String>,
    ) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        if self.remote_tag_root_moved(root) {
            return;
        }
        let tags = match tags {
            Ok(tags) => tags,
            Err(err) => {
                self.status = format!("Remote tags: {}", crate::git::result_line(&err));
                return;
            }
        };
        self.status.clear();
        self.remote_tag_root = Some(root.to_path_buf());
        let rows = tags
            .into_iter()
            .map(|tag| ListRow {
                id: format!("{remote}\t{tag}"),
                label: tag,
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(
                ListPurpose::DeleteRemoteTag,
                format!("Delete Remote Tag from {remote}"),
                rows,
            ),
            &format!("No tags on {remote}"),
        );
    }

    pub(super) fn open_stash_picker(&mut self, purpose: crate::widgets::list_picker::ListPurpose) {
        use crate::widgets::list_picker::{ListPicker, ListRow};
        let rows: Vec<ListRow> = match crate::git::list_stashes(&self.scm_root()) {
            Ok(stashes) => stashes
                .into_iter()
                .map(|s| ListRow {
                    id: s.index.to_string(),
                    label: s.label(),
                })
                .collect(),
            Err(err) => {
                self.status = format!("Stashes: {err}");
                return;
            }
        };
        let title = match purpose {
            crate::widgets::list_picker::ListPurpose::StashApply => "Apply Stash",
            crate::widgets::list_picker::ListPurpose::StashDrop => "Drop Stash",
            crate::widgets::list_picker::ListPurpose::StashView => "View Stash",
            _ => "Pop Stash",
        };
        self.open_list_picker(
            ListPicker::new(purpose, title, rows),
            "No stashes to choose",
        );
    }

    pub(super) fn open_tag_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows: Vec<ListRow> = match crate::git::list_tags(&self.scm_root()) {
            Ok(tags) => tags
                .into_iter()
                .map(|t| ListRow {
                    id: t.clone(),
                    label: t,
                })
                .collect(),
            Err(err) => {
                self.status = format!("Tags: {err}");
                return;
            }
        };
        self.open_list_picker(
            ListPicker::new(ListPurpose::DeleteTag, "Delete Tag", rows),
            "No tags to delete",
        );
    }

    pub(super) fn open_remote_picker(&mut self, purpose: crate::widgets::list_picker::ListPurpose) {
        use crate::widgets::list_picker::{ListPicker, ListRow};
        let rows: Vec<ListRow> = match crate::git::list_remotes(&self.scm_root()) {
            Ok(remotes) => remotes
                .into_iter()
                .map(|r| ListRow {
                    id: r.name.clone(),
                    label: format!("{}  {}", r.name, r.url),
                })
                .collect(),
            Err(err) => {
                self.status = format!("Remotes: {err}");
                return;
            }
        };
        self.open_list_picker(
            ListPicker::new(purpose, "Remove Remote", rows),
            "No remotes configured",
        );
    }

    pub(super) fn open_push_to_remote_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows: Vec<ListRow> = match crate::git::list_remotes(&self.scm_root()) {
            Ok(remotes) => remotes
                .into_iter()
                .map(|r| ListRow {
                    id: r.name.clone(),
                    label: format!("{}  {}", r.name, r.url),
                })
                .collect(),
            Err(err) => {
                self.status = format!("Remotes: {err}");
                return;
            }
        };
        self.open_list_picker(
            ListPicker::new(ListPurpose::PushToRemote, "Push to Remote", rows),
            "No remotes configured",
        );
    }

    /// Resolve the branch-picker selection per its open purpose.
    pub(super) fn apply_branch_picker_selection(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let Some(picker) = self.branch_picker.as_ref() else {
            return;
        };
        match self.branch_purpose {
            BranchPurpose::Checkout => {
                if let Some(action) = picker.selected_action() {
                    self.apply_branch_action(action);
                } else {
                    self.close_branch_picker();
                }
            }
            BranchPurpose::CreateFrom => {
                let Some(base) = picker.selected_existing_branch() else {
                    return;
                };
                self.close_branch_picker();
                self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
                    crate::widgets::input_prompt::InputPurpose::CreateBranchFrom { base },
                    "Create Branch from",
                    "New branch name",
                ));
            }
            BranchPurpose::Delete => {
                let Some(name) = picker.selected_existing_branch() else {
                    return;
                };
                self.close_branch_picker();
                let r = crate::git::delete_branch(&self.scm_root(), &name);
                self.run_scm_op("branch -d", r, "Deleted branch", "Delete branch");
            }
            BranchPurpose::Merge => {
                let Some(name) = picker.selected_existing_branch() else {
                    return;
                };
                self.close_branch_picker();
                let r = crate::git::merge_branch(&self.scm_root(), &name);
                self.run_scm_op("merge", r, "Merged", "Merge");
            }
            BranchPurpose::Rebase => {
                let Some(name) = picker.selected_existing_branch() else {
                    return;
                };
                self.close_branch_picker();
                let r = crate::git::rebase_branch(&self.scm_root(), &name);
                self.run_scm_op("rebase", r, "Rebased", "Rebase");
            }
        }
    }

    /// Open the staged-changes diff (`git diff --staged`) in a read-only
    /// editor tab. Mirrors VS Code's "View Staged Changes" — users hit
    /// the dropdown, click View Staged Changes, and the editor tab pops up
    /// with every `+`/`-` line of the staged hunks highlighted.
    pub fn view_staged_diff_source_control(&mut self) {
        let raw = match crate::git::diff_staged(&self.scm_root()) {
            Ok(r) => r,
            Err(err) => {
                let why = self.git_error_line("diff --staged", &err);
                self.source_control.commit_feedback = Some(format!("diff failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Staged Changes failed: {why}");
                return;
            }
        };
        let label = std::path::PathBuf::from("git diff --staged");
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open staged diff: {err}");
            return;
        }
        self.tag_open_diff(crate::widgets::diff::DiffSource::GitCommand {
            root: self.scm_root(),
            kind: crate::widgets::diff::GitDiffKind::Staged,
        });
        self.source_control.commit_feedback = None;
        self.status = String::from("Showing git diff --staged");
        self.focus_pane(Pane::Editor);
    }

    /// Open `git diff <default-branch>` in a read-only editor tab. The
    /// default branch is resolved via `git::default_branch` (origin/HEAD
    /// first, then common local names). If resolution fails the error
    /// surfaces in the commit-feedback line so the user can fix the
    /// upstream-HEAD config instead of getting a silent no-op.
    pub fn view_default_branch_diff_source_control(&mut self) {
        let branch = match crate::git::default_branch(&self.scm_root()) {
            Ok(b) => b,
            Err(err) => {
                self.source_control.commit_feedback = Some(err.clone());
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Default branch: {err}");
                return;
            }
        };
        let raw = match crate::git::diff_against_branch(&self.scm_root(), &branch) {
            Ok(r) => r,
            Err(err) => {
                let why = self.git_error_line(&format!("diff {branch}"), &err);
                self.source_control.commit_feedback = Some(format!("diff failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Changes vs {branch} failed: {why}");
                return;
            }
        };
        // Name the base the diff starts from (#1444): the merge base, or
        // the branch tip when the two share no history.
        let (title, said) = match crate::git::merge_base(&self.scm_root(), &branch) {
            Some(base) => (
                format!(
                    "git diff {} (merge base with {branch})",
                    &base[..base.len().min(7)]
                ),
                format!("Showing changes since this branch left {branch}"),
            ),
            None => (
                format!("git diff {branch}"),
                format!("Showing git diff {branch}: no history in common, so against its tip"),
            ),
        };
        let label = std::path::PathBuf::from(title);
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open diff vs {branch}: {err}");
            return;
        }
        self.tag_open_diff(crate::widgets::diff::DiffSource::GitCommand {
            root: self.scm_root(),
            kind: crate::widgets::diff::GitDiffKind::AgainstBranch(branch.clone()),
        });
        self.default_branch_label = Some(branch.clone());
        self.source_control.commit_feedback = None;
        self.status = said;
        self.focus_pane(Pane::Editor);
    }

    /// Open `git diff HEAD~1` (working tree versus the commit before HEAD)
    /// in a read-only editor tab. Mirrors "View Changes vs <default>" but
    /// anchored on the previous commit, so the user can review everything
    /// that changed since the last commit. A single-commit repo has no
    /// `HEAD~1`; that error surfaces in the commit-feedback line rather
    /// than as a silent no-op.
    pub fn view_previous_commit_diff_source_control(&mut self) {
        let raw = match crate::git::diff_previous_commit(&self.scm_root()) {
            Ok(r) => r,
            Err(err) => {
                let why = self.git_error_line("diff HEAD~1", &err);
                self.source_control.commit_feedback = Some(format!("diff failed: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("View Changes vs previous failed: {why}");
                return;
            }
        };
        let label = std::path::PathBuf::from("git diff HEAD~1");
        if let Err(err) = self.editor.open_git_diff_side_by_side(&label, &raw) {
            self.source_control.commit_feedback = Some(format!("open failed: {err}"));
            self.source_control.commit_feedback_is_error = true;
            self.status = format!("Could not open diff vs previous: {err}");
            return;
        }
        self.tag_open_diff(crate::widgets::diff::DiffSource::GitCommand {
            root: self.scm_root(),
            kind: crate::widgets::diff::GitDiffKind::PreviousCommit,
        });
        self.source_control.commit_feedback = None;
        self.status = String::from("Showing git diff HEAD~1");
        self.focus_pane(Pane::Editor);
    }

    pub fn commit_and_push_source_control(&mut self) {
        // Checked before committing: a commit whose push is then refused
        // would leave the user thinking it had been pushed.
        if self.git_net_busy() {
            return;
        }
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback = Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return;
        }
        if self.hold_for_unsaved(ScmWrite::CommitAndPush) {
            return;
        }
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || crate::git::commit(&root, &committed),
            |app, r| {
                app.log_git("commit", &r);
                let commit_summary = match r {
                    Ok(s) => crate::git::headline(&s).to_string(),
                    Err(err) => {
                        app.report_commit_failure(&err);
                        return;
                    }
                };
                // Mid-rebase, push only once the rebase is back on its
                // branch, never from the detached HEAD it works on.
                if !app.continue_rebase_after_commit() {
                    return;
                }
                let root = app.scm_root();
                app.spawn_git_net("push", move || {
                    let r = crate::git::push_or_publish(&root);
                    Box::new(move |app: &mut App| app.finish_commit_and_push(commit_summary, r))
                });
                app.active_git_bypass_debounce();
                app.refresh_git_status_debounced();
                app.refresh_source_control();
            },
        );
    }

    /// Report Commit & Push on one line (#858): the commit's headline and
    /// the push's ref update, or why the push failed; the Git Output log
    /// keeps git push's whole text.
    pub(super) fn finish_commit_and_push(
        &mut self,
        commit_summary: String,
        push: Result<String, String>,
    ) {
        self.log_git("push", &push);
        let push = push
            .map(|out| crate::git::summary_line(&out))
            .map_err(|err| crate::git::error_line(&err));
        match push {
            Ok(push_summary) => {
                let combined = if push_summary.is_empty() {
                    commit_summary
                } else {
                    format!("{commit_summary} | pushed: {push_summary}")
                };
                self.source_control.commit_feedback = Some(combined.clone());
                self.source_control.commit_feedback_is_error = false;
                self.status = format!("Committed & pushed: {combined}");
            }
            Err(err) => {
                self.source_control.commit_feedback =
                    Some(format!("commit ok; push failed: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Commit ok; push failed: {err}");
            }
        }
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    pub fn stage_source_control_entry(&mut self, entry_idx: usize) {
        if self.repo_git_busy() {
            return;
        }
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        match crate::git::stage_path(&self.scm_root(), &entry.path) {
            Ok(()) => {
                self.status = format!("Staged {}", entry.path);
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                let why = self.git_error_line("add", &err);
                self.status = format!("Stage failed: {why}");
                self.source_control.commit_feedback = Some(why);
                self.source_control.commit_feedback_is_error = true;
            }
        }
    }

    /// The workspace-relative path + single-hunk patch for the hunk under
    /// the diff caret. Sets a status message and returns `None` when the
    /// active tab isn't a working-tree diff or no hunk is at the cursor.
    pub(super) fn diff_hunk_patch_at_caret(&mut self) -> Option<(String, String)> {
        let built = match self.editor.diff.as_ref() {
            None => self.editor_hunk_patch_at_caret().map_err(str::to_string),
            Some(diff) if !diff.left_is_git_head => {
                Err("Hunk actions need a working-tree diff from Source Control".to_string())
            }
            Some(diff) if diff.right_is_unsaved => {
                Err("Save the file first: hunk actions apply to the file on disk".to_string())
            }
            // The patch is built from the decoded text, which would put UTF-8
            // bytes into a blob stored in another encoding (#1242).
            Some(diff) if diff.text_encoding != encoding_rs::UTF_8 => Err(format!(
                "Hunk actions work on UTF-8 files only; this diff is {}: stage the whole file",
                diff.text_encoding.name()
            )),
            Some(diff) => match self.repo_relative_path(&diff.right_path) {
                None => Err("File is outside the repository".to_string()),
                Some(rel) => match diff.hunk_range_at(diff.action_row()) {
                    None => Err("No change hunk at the cursor".to_string()),
                    Some(range) => {
                        let patch = diff.hunk_patch(&rel, range);
                        Ok((rel, patch))
                    }
                },
            },
        };
        match built {
            Ok(pair) => Some(pair),
            Err(msg) => {
                self.status = msg.to_string();
                None
            }
        }
    }

    /// Stage / Unstage / Revert Hunk from an editor tab (#1352): the hunk
    /// whose change bar the caret is on, as a patch of the file on disk
    /// against HEAD, the same pair the gutter's bars come from. Refused
    /// with unsaved edits, since the patch is applied to the saved file.
    pub(super) fn editor_hunk_patch_at_caret(&mut self) -> Result<(String, String), &'static str> {
        let Some(path) = self.editor.path.clone() else {
            return Err("Hunk actions need a file in a git repository");
        };
        if self.editor.dirty {
            return Err("Save the file first: hunk actions apply to the file on disk");
        }
        let row = self.editor.cursor_row;
        let Some(mark) = self.editor.current_git_mark_at(row) else {
            return Err("No change at the cursor");
        };
        let rel = self
            .repo_relative_path(&path)
            .ok_or("File is outside the repository")?;
        let head = crate::git::read_file_at_head(&self.scm_root(), &rel)
            .map_err(|_| "This file has no version in HEAD")?;
        let disk = std::fs::read_to_string(&path).map_err(|_| "Could not read the file")?;
        let data = crate::widgets::diff::DiffData::build_with_byte_check(
            PathBuf::from(&rel),
            path,
            head.lines().map(str::to_string).collect(),
            disk.lines().map(str::to_string).collect(),
            Some(&head),
            Some(&disk),
        );
        // The diff row showing this line; a deletion bar sits on the line
        // after the removed run, so its hunk is the row just above.
        use crate::widgets::diff::DiffRow;
        let at = data
            .rows
            .iter()
            .position(|r| match *r {
                DiffRow::Equal { right, .. }
                | DiffRow::Added { right }
                | DiffRow::Replaced { right, .. } => right == row,
                DiffRow::Removed { .. } => false,
            })
            .map(|i| {
                if mark == crate::widgets::editor::GitMark::Deleted {
                    i.saturating_sub(1)
                } else {
                    i
                }
            })
            .ok_or("No change at the cursor")?;
        let range = data.hunk_range_at(at).ok_or("No change at the cursor")?;
        Ok((rel.clone(), data.hunk_patch(&rel, range)))
    }

    /// The workspace-relative path + patch for the SELECTED lines of the
    /// open working-tree diff (VS Code's "Stage Selected Ranges"), or
    /// `None` when there is no selection spanning a changed row, so the
    /// caller falls back to the whole-hunk action.
    pub(super) fn diff_selected_lines_patch(&self) -> Option<(String, String)> {
        let diff = self.editor.diff.as_ref()?;
        // Not UTF-8: left to the hunk path, which refuses it with a reason.
        if !diff.left_is_git_head
            || diff.right_is_unsaved
            || diff.text_encoding != encoding_rs::UTF_8
        {
            return None;
        }
        let sel = diff.selection?;
        if !sel.has_area() {
            return None;
        }
        let rel = self.repo_relative_path(&diff.right_path)?;
        let (start, end) = sel.normalized();
        let patch = diff.selected_lines_patch(&rel, (start.0, end.0))?;
        Some((rel, patch))
    }

    /// `path` relative to the repository top level, where `git apply` runs
    /// (#139's convention). Relative to the WORKSPACE, a workspace opened on
    /// `repo/sub` sent `x` for `sub/x`, and every hunk action failed or hit
    /// a same-named file at the top level.
    pub(super) fn repo_relative_path(&self, path: &Path) -> Option<String> {
        let root = self.scm_root();
        let rel = match path.strip_prefix(&root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => path
                .canonicalize()
                .ok()?
                .strip_prefix(root.canonicalize().ok()?)
                .ok()?
                .to_path_buf(),
        };
        Some(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// Stage only the hunk under the diff caret (`git apply --cached`).
    pub fn stage_hunk_at_caret(&mut self) {
        // A selection spanning changed rows narrows the action to those
        // lines (VS Code's "Stage Selected Ranges"); without one the whole
        // hunk under the caret is staged, as before.
        if let Some((rel, patch)) = self.diff_selected_lines_patch() {
            match crate::git::apply_patch(&self.scm_root(), &patch, true, false) {
                Ok(_) => {
                    self.status = format!("Staged selected lines in {rel}");
                    self.refresh_after_hunk_op();
                }
                Err(err) => self.status = format!("Stage selected lines failed: {err}"),
            }
            return;
        }
        let Some((rel, patch)) = self.diff_hunk_patch_at_caret() else {
            return;
        };
        match crate::git::apply_patch(&self.scm_root(), &patch, true, false) {
            Ok(_) => {
                self.status = format!("Staged hunk in {rel}");
                self.refresh_after_hunk_op();
            }
            Err(err) => self.status = format!("Stage hunk failed: {err}"),
        }
    }

    /// Remove only the hunk under the diff caret from the index
    /// (`git apply --cached -R`). Errors surface verbatim, e.g. when the
    /// hunk was never staged.
    pub fn unstage_hunk_at_caret(&mut self) {
        // Same selection-first rule as staging: the reverse patch removes
        // exactly the selected lines from the index.
        if let Some((rel, patch)) = self.diff_selected_lines_patch() {
            match crate::git::apply_patch(&self.scm_root(), &patch, true, true) {
                Ok(_) => {
                    self.status = format!("Unstaged selected lines in {rel}");
                    self.refresh_after_hunk_op();
                }
                Err(err) => self.status = format!("Unstage selected lines failed: {err}"),
            }
            return;
        }
        let Some((rel, patch)) = self.diff_hunk_patch_at_caret() else {
            return;
        };
        match crate::git::apply_patch(&self.scm_root(), &patch, true, true) {
            Ok(_) => {
                self.status = format!("Unstaged hunk in {rel}");
                self.refresh_after_hunk_op();
            }
            Err(err) => self.status = format!("Unstage hunk failed: {err}"),
        }
    }

    /// Arm the revert-hunk confirm popup for the hunk under the caret.
    pub fn request_revert_hunk_at_caret(&mut self) {
        if let Some((rel_path, patch)) = self.diff_hunk_patch_at_caret() {
            self.pending_revert_hunk = Some(PendingRevertHunk { rel_path, patch });
        }
    }

    pub fn confirm_pending_revert_hunk(&mut self) {
        let Some(pr) = self.pending_revert_hunk.take() else {
            return;
        };
        match crate::git::apply_patch(&self.scm_root(), &pr.patch, false, true) {
            Ok(_) => {
                self.status = format!("Reverted hunk in {}", pr.rel_path);
                self.refresh_after_hunk_op();
                // Reverted from an editor tab (#1352): show the restored text
                // now rather than on the next file-system sweep.
                if self.editor.diff.is_none() && self.editor.reload_if_clean().is_some() {
                    self.sync_open_file_poll_mtime();
                }
                // The revert rewrote the working file; force the HEAD-side rebuild
                // of the open view so it refreshes even if the rewrite landed
                // within the stamp's granularity. Forced by the root the VIEW is
                // tagged with, not `scm_root()` re-read now: the tag may predate the
                // worker's first status reply and hold the workspace root.
                let force_root = self
                    .editor
                    .diff
                    .as_ref()
                    .and_then(|d| match &d.source {
                        crate::widgets::diff::DiffSource::HeadVsWorking { root, .. } => {
                            Some(root.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| self.scm_root());
                self.refresh_open_diff_views(
                    true,
                    std::slice::from_ref(&force_root),
                    &std::collections::BTreeSet::new(),
                );
            }
            Err(err) => self.status = format!("Revert hunk failed: {err}"),
        }
    }

    pub fn cancel_pending_revert_hunk(&mut self) {
        self.pending_revert_hunk = None;
    }

    pub(super) fn refresh_after_hunk_op(&mut self) {
        self.active_git_bypass_debounce();
        self.refresh_git_status_debounced();
        self.refresh_source_control();
    }

    pub fn request_discard_source_control_entry(&mut self, entry_idx: usize) {
        use crate::git::ChangeKind;
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        let untracked = matches!(entry.kind, ChangeKind::Untracked);
        let staged = matches!(
            entry.kind,
            ChangeKind::StagedAdded
                | ChangeKind::StagedModified
                | ChangeKind::StagedDeleted
                | ChangeKind::StagedRenamed
        );
        self.pending_discard = Some(PendingDiscard {
            rel_path: entry.path,
            untracked,
            staged,
        });
    }

    pub fn confirm_pending_discard(&mut self) {
        let Some(pd) = self.pending_discard.take() else {
            return;
        };
        match crate::git::discard_path(&self.scm_root(), &pd.rel_path, pd.untracked, pd.staged) {
            Ok(()) => {
                self.status = format!("Discarded {}", pd.rel_path);
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
            }
            Err(err) => {
                let why = self.git_error_line("checkout -- (discard)", &err);
                self.status = format!("Discard failed: {why}");
                self.source_control.commit_feedback = Some(why);
                self.source_control.commit_feedback_is_error = true;
            }
        }
    }

    pub fn cancel_pending_discard(&mut self) {
        self.pending_discard = None;
    }

    pub(super) fn commit_source_control(&mut self) {
        if self.repo_git_busy() {
            return;
        }
        let message = self.source_control.message.trim().to_string();
        if message.is_empty() {
            self.source_control.commit_feedback = Some(String::from("Empty commit message"));
            self.source_control.commit_feedback_is_error = true;
            return;
        }
        if self.hold_for_unsaved(ScmWrite::Commit) {
            return;
        }
        let root = self.scm_root();
        let committed = message.clone();
        self.spawn_commit(
            message,
            move || crate::git::commit(&root, &committed),
            |app, r| {
                app.log_git("commit", &r);
                match r {
                    Ok(summary) => {
                        let headline = crate::git::headline(&summary);
                        app.source_control.commit_feedback = Some(headline.to_string());
                        app.source_control.commit_feedback_is_error = false;
                        app.status = format!("Committed: {headline}");
                        app.active_git_bypass_debounce();
                        app.refresh_git_status_debounced();
                        app.refresh_source_control();
                        app.continue_rebase_after_commit();
                    }
                    Err(err) => app.report_commit_failure(&err),
                }
            },
        );
    }

    /// Run a commit on the git worker (#1153). The repository's hooks
    /// (pre-commit, commit-msg) can run for minutes; inline, the whole UI
    /// froze until they ended and every key pressed meanwhile fired at once.
    /// The message stays in the box until the commit lands, and is cleared
    /// then only if it is still the one committed. `done` applies the
    /// result on the UI thread.
    pub(super) fn spawn_commit(
        &mut self,
        message: String,
        commit: impl FnOnce() -> Result<String, String> + Send + 'static,
        done: impl FnOnce(&mut App, Result<String, String>) + Send + 'static,
    ) {
        if self.git_net_busy() {
            return;
        }
        self.spawn_git_net("commit", move || {
            let r = commit();
            Box::new(move |app: &mut App| {
                if r.is_ok() && app.source_control.message.trim() == message.trim() {
                    app.source_control.clear_after_commit();
                }
                done(app, r)
            })
        });
        let running = String::from("Committing\u{2026} (running hooks)");
        self.source_control.commit_feedback = Some(running.clone());
        self.source_control.commit_feedback_is_error = false;
        self.status = running;
    }

    /// A refused `git commit` (#858): the panel and the status bar name why
    /// on one line ("nothing to commit, working tree clean", not the "On
    /// branch main" git prints first); the Git Output log has the rest.
    pub(super) fn report_commit_failure(&mut self, err: &str) {
        let why = crate::git::error_line(err);
        self.source_control.commit_feedback = Some(why.clone());
        self.source_control.commit_feedback_is_error = true;
        self.status = format!("Commit failed: {why}");
    }

    /// Open the Checkout / Create Branch picker, seeded with the repo's
    /// branches (most-recently-committed first). A one-shot `git
    /// for-each-ref` runs here, off the render path — same discipline as
    /// the zoxide jumper. Outside a git repo there is nothing to pick, so
    /// we report that instead of opening an empty box.
    pub(super) fn open_branch_picker(&mut self) {
        if self.branch_picker.is_some() {
            return;
        }
        self.commit_menu_open = false;
        match crate::git::list_branches(&self.scm_root()) {
            Ok(branches) => {
                let n = branches.len();
                self.branch_picker =
                    Some(crate::widgets::branch_picker::BranchPicker::new(branches));
                self.overlays.popup_clear.request();
                self.status =
                    format!("Branches: {n} — type to filter or name a new branch, Esc to close");
            }
            Err(err) => {
                self.source_control.commit_feedback = Some(format!("branches: {err}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Branch picker: {err}");
            }
        }
    }

    pub(super) fn close_branch_picker(&mut self) {
        if self.branch_picker.take().is_some() {
            self.overlays.popup_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Run the action the highlighted picker row resolves to: switch to an
    /// existing branch, or create-and-switch a new one. The picker closes
    /// on success; on failure it stays open with the error in the SC
    /// feedback line so the user can amend the name, unless local changes
    /// blocked the switch: then Stash & Checkout is offered (#925). True
    /// when the switch happened.
    pub(super) fn apply_branch_action(
        &mut self,
        action: crate::widgets::branch_picker::BranchAction,
    ) -> bool {
        use crate::widgets::branch_picker::BranchAction;
        let (result, verb, label, name) = match &action {
            BranchAction::Checkout(name) => (
                crate::git::checkout_branch(&self.scm_root(), name),
                "Switched to",
                "switch",
                name,
            ),
            BranchAction::Track { remote_ref, name } => (
                crate::git::track_remote_branch(&self.scm_root(), remote_ref, name),
                "Switched to",
                "switch --track",
                name,
            ),
            BranchAction::Create(name) => (
                crate::git::create_branch(&self.scm_root(), name),
                "Created",
                "switch -c",
                name,
            ),
        };
        self.log_git(label, &result);
        match result {
            Ok(summary) => {
                // One line (#858): git's result, or, when git said only which
                // edits it carried over (`M\tpath`) or how the branch stands
                // against its upstream, where the switch went.
                let summary = crate::git::result_line(&summary);
                let (feedback, status) = if summary.is_empty() {
                    let line = format!("{verb} {name}");
                    (line.clone(), line)
                } else {
                    (summary.clone(), format!("{verb}: {summary}"))
                };
                self.close_branch_picker();
                self.source_control.commit_feedback = Some(feedback);
                self.source_control.commit_feedback_is_error = false;
                self.status = status;
                self.active_git_bypass_debounce();
                self.refresh_git_status_debounced();
                self.refresh_source_control();
                true
            }
            Err(err) => {
                if crate::git::local_changes_block_switch(&err) {
                    self.offer_stash_and_switch(action);
                }
                let why = crate::git::error_line(&err);
                self.source_control.commit_feedback = Some(format!("branch: {why}"));
                self.source_control.commit_feedback_is_error = true;
                self.status = format!("Branch: {why}");
                false
            }
        }
    }

    /// A switch git refused because it would overwrite local changes
    /// (#925): instead of a dead end, offer what VS Code does, Stash &
    /// Checkout and Migrate Changes, in place of the branch picker.
    pub(super) fn offer_stash_and_switch(
        &mut self,
        action: crate::widgets::branch_picker::BranchAction,
    ) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let name = action.branch_name().to_string();
        let rows = vec![
            ListRow {
                id: String::from("stash"),
                label: format!("Stash & Checkout: stash your changes, then switch to {name}"),
            },
            ListRow {
                id: String::from("migrate"),
                label: format!("Migrate Changes: switch to {name} and bring your changes"),
            },
        ];
        self.close_branch_picker();
        let root = self.scm_root();
        let from = crate::git::current_branch(&root);
        self.blocked_switch = Some((root, from, action));
        let title = format!("Your changes block the switch to {name}");
        self.open_list_picker(ListPicker::new(ListPurpose::BlockedSwitch, title, rows), "");
    }

    /// Stash & Checkout or Migrate Changes picked (#925), past the
    /// unsaved-changes prompt as any stash is (#1438): git stashes the files
    /// on disk, so an edit still only in a tab would be left out, and the
    /// switch would change the file under it.
    pub(super) fn stash_and_switch_picked(&mut self, migrate: bool) {
        let op = if migrate {
            ScmWrite::MigrateAndSwitch
        } else {
            ScmWrite::StashAndSwitch
        };
        if self.hold_for_unsaved(op) {
            return;
        }
        let Some((root, from, action)) = self.blocked_switch.take() else {
            return;
        };
        // The prompt, or a formatter it waits on, leaves time to switch
        // branches by hand: the changes to stash are then another branch's.
        if crate::git::current_branch(&root) != from {
            let what = if migrate {
                "Migrate Changes"
            } else {
                "Stash & Checkout"
            };
            let line = format!("{what} cancelled: the branch changed since the switch was refused");
            self.source_control.commit_feedback = Some(line.clone());
            self.source_control.commit_feedback_is_error = true;
            self.status = line;
            return;
        }
        self.stash_and_switch(root, action, migrate);
    }

    /// Stash & Checkout, or with `migrate` Migrate Changes (#925): stash the
    /// changes that blocked the switch, switch, and for a migration pop them
    /// onto the new branch. A pop that stops on conflicts keeps the stash,
    /// as git does; a switch that still fails leaves the changes stashed.
    pub(super) fn stash_and_switch(
        &mut self,
        root: PathBuf,
        action: crate::widgets::branch_picker::BranchAction,
        migrate: bool,
    ) {
        if self.repo_git_busy() || root != self.scm_root() {
            return;
        }
        let name = action.branch_name().to_string();
        // The stash this makes is the one to pop, not an older one left
        // behind should there be nothing to stash by now, nor a newer one a
        // checkout hook pushes during the switch: it is popped by its commit.
        let before = crate::git::stash_oid(&root, 0).ok();
        let stash = crate::git::stash_push(&root, None);
        self.log_git("stash push", &stash);
        if let Err(err) = stash {
            let line = crate::git::failure_line("Stash", &err);
            self.source_control.commit_feedback = Some(line.clone());
            self.source_control.commit_feedback_is_error = true;
            self.status = line;
            return;
        }
        let created = crate::git::stash_oid(&root, 0)
            .ok()
            .filter(|oid| before.as_ref() != Some(oid));
        let switched = self.apply_branch_action(action);
        let Some(created) = created else {
            return;
        };
        let (line, is_error) = match (switched, migrate) {
            (false, _) => {
                let line = format!("{} \u{b7} your changes are in the stash", self.status);
                (line, true)
            }
            (true, false) => (
                format!("Stashed your changes and switched to {name}"),
                false,
            ),
            (true, true) => {
                let pop = crate::git::stash_pop_oid(&root, &created);
                self.log_git("stash pop", &pop);
                match pop {
                    Ok(_) => (format!("Switched to {name} with your changes"), false),
                    Err(err) => {
                        let why = crate::git::failure_line("Migrate changes", &err);
                        (format!("Switched to {name} \u{b7} {why}"), true)
                    }
                }
            }
        };
        self.source_control.commit_feedback = Some(line.clone());
        self.source_control.commit_feedback_is_error = is_error;
        self.status = line;
        self.refresh_source_control();
    }

    pub(super) fn handle_branch_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.branch_picker.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_branch_picker(),
            (KeyCode::Enter, _) => self.apply_branch_picker_selection(),
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            (KeyCode::Left, _) => picker.move_cursor_left(),
            (KeyCode::Right, _) => picker.move_cursor_right(),
            (KeyCode::Home, _) => picker.move_cursor_home(),
            (KeyCode::End, _) => picker.move_cursor_end(),
            (KeyCode::Backspace, _) => picker.pop_char(),
            (KeyCode::Delete, _) => picker.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
            }
            _ => {}
        }
    }

    pub(super) fn handle_branch_picker_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.branch_picker.as_mut() else {
            return;
        };
        let inside = rect_contains(picker.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    picker.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    picker.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_branch_picker();
            }
            // Click a row to act on it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.apply_branch_picker_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_branch_picker(&mut self, frame: &mut ratatui::Frame) {
        let sidebar = self.last_sidebar_area;
        let full = frame.area();
        let theme = self.theme;
        let Some(picker) = self.branch_picker.as_mut() else {
            return;
        };
        // Anchor over the sidebar column like the jumper (it is a Source
        // Control action), widening rightward for readable branch names;
        // fall back to a centered box when the sidebar is collapsed.
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
        crate::widgets::branch_picker::render_branch_picker(
            picker,
            rect,
            frame.buffer_mut(),
            theme,
        );
    }

    /// Render the Source Control "⋯" actions menu under the header pill.
    pub(super) fn render_scm_menu(&mut self, frame: &mut ratatui::Frame) {
        if !self.scm_menu.open || self.sidebar_view != SidebarView::SourceControl {
            self.scm_menu.close();
            return;
        }
        let anchor = self.source_control.header_more_btn;
        if anchor.width == 0 {
            return;
        }
        let screen = frame.area();
        self.scm_menu.hover = self.pointer_cell;
        crate::widgets::scm_menu::render(
            &mut self.scm_menu,
            anchor,
            screen,
            frame.buffer_mut(),
            self.theme,
        );
    }

    pub(super) fn toggle_inline_blame(&mut self) {
        self.inline_blame_enabled = !self.inline_blame_enabled;
        self.editor.blame_enabled = self.inline_blame_enabled;
        // Re-enabling refetches (the sync marker was left set); disabling
        // keeps the data so a later re-enable can be instant if unchanged.
        if self.inline_blame_enabled {
            self.blame_fetched = None;
        }
        self.status = if self.inline_blame_enabled {
            String::from("Inline Blame: on")
        } else {
            String::from("Inline Blame: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inline_blame(self.inline_blame_enabled));
        }
    }

    /// The right-click menu of a COMMITS graph row (#1627), VS Code's
    /// Source Control Graph menu: start a cherry-pick or revert from the
    /// commit, check it out, branch, tag or reset there, or copy it.
    pub(super) fn commit_graph_menu(
        &self,
        origin: (u16, u16),
        commit: &crate::git::GraphCommit,
    ) -> ContextMenu {
        use crate::git::ResetMode;
        let item = |label: &str, action: CommitAction| MenuEntry::Item {
            label: label.to_string(),
            action: MenuAction::CommitAction {
                action,
                hash: commit.hash.clone(),
                short: commit.short_hash.clone(),
            },
        };
        let items = vec![
            item("Cherry-Pick", CommitAction::CherryPick),
            item("Revert", CommitAction::Revert),
            MenuEntry::Separator,
            item("Checkout (Detached)", CommitAction::CheckoutDetached),
            item("Create Branch Here...", CommitAction::CreateBranch),
            item("Create Tag Here...", CommitAction::CreateTag),
            MenuEntry::Submenu {
                label: String::from("Reset Current Branch to Here"),
                items: vec![
                    item(
                        "Soft (keep changes staged)",
                        CommitAction::Reset(ResetMode::Soft),
                    ),
                    item(
                        "Mixed (keep changes unstaged)",
                        CommitAction::Reset(ResetMode::Mixed),
                    ),
                    item(
                        "Hard (discard changes)",
                        CommitAction::Reset(ResetMode::Hard),
                    ),
                ],
            },
            MenuEntry::Separator,
            item("Copy Commit Hash", CommitAction::CopyHash),
            item("Copy Commit Message", CommitAction::CopyMessage),
        ];
        ContextMenu {
            origin,
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
        }
    }

    /// Run one entry of [`App::commit_graph_menu`] on `hash`. A cherry-pick
    /// or revert that conflicts stops like one started in a terminal, so
    /// Git: Continue / Skip / Abort take it from there; a hard reset asks
    /// first, the way Undo Last Commit does for a pushed commit.
    pub(super) fn run_commit_action(&mut self, action: CommitAction, hash: &str, short: &str) {
        use crate::git::ResetMode;
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let root = self.scm_root();
        if action != CommitAction::Reset(ResetMode::Hard) {
            self.reset_hard_armed = None;
        }
        match action {
            // Both commit, so signing and the commit hooks run: on the git
            // worker, like a plain commit (#1153), not in the click.
            CommitAction::CherryPick => {
                let (hash, ok) = (hash.to_string(), format!("Cherry-picked {short}"));
                self.spawn_git_net("cherry-pick", move || {
                    let r = crate::git::cherry_pick(&root, &hash);
                    Box::new(move |app: &mut App| {
                        app.run_scm_op("cherry-pick", r, &ok, "Cherry-pick")
                    })
                });
            }
            CommitAction::Revert => {
                let (hash, ok) = (hash.to_string(), format!("Reverted {short}"));
                self.spawn_git_net("revert", move || {
                    let r = crate::git::revert_commit(&root, &hash);
                    Box::new(move |app: &mut App| app.run_scm_op("revert", r, &ok, "Revert"))
                });
            }
            // A big checkout or a slow `post-checkout` hook would freeze
            // the click, so this and reset run on the git worker too.
            CommitAction::CheckoutDetached => {
                let (hash, ok) = (hash.to_string(), format!("Checked out {short} (detached)"));
                self.spawn_git_net("switch --detach", move || {
                    let r = crate::git::checkout_detached(&root, &hash);
                    Box::new(move |app: &mut App| {
                        app.run_scm_op("switch --detach", r, &ok, "Checkout")
                    })
                });
            }
            CommitAction::CreateBranch => self.open_input_prompt(InputPrompt::new(
                InputPurpose::CreateBranchFrom {
                    base: hash.to_string(),
                },
                format!("Create Branch at {short}"),
                "New branch name",
            )),
            CommitAction::CreateTag => self.open_input_prompt(InputPrompt::new(
                InputPurpose::CreateTagAt {
                    target: hash.to_string(),
                },
                format!("Create Tag at {short}"),
                "Tag name (e.g. v1.0.0)",
            )),
            CommitAction::Reset(mode) => {
                let armed = (mode == ResetMode::Hard).then(|| {
                    (
                        root.clone(),
                        hash.to_string(),
                        crate::git::worktree_fingerprint(&root),
                    )
                });
                if armed.is_some() && self.reset_hard_armed != armed {
                    self.reset_hard_armed = armed;
                    let warning = format!(
                        "Reset --hard to {short} discards every uncommitted change. Choose Hard again to reset anyway"
                    );
                    self.source_control.commit_feedback = Some(warning.clone());
                    self.source_control.commit_feedback_is_error = true;
                    self.status = warning;
                    return;
                }
                self.reset_hard_armed = None;
                let (hash, ok) = (hash.to_string(), format!("Reset to {short}"));
                self.spawn_git_net("reset", move || {
                    let r = crate::git::reset_to(&root, &hash, mode);
                    Box::new(move |app: &mut App| app.run_scm_op("reset", r, &ok, "Reset"))
                });
            }
            CommitAction::CopyHash => {
                let landing = self.copy_where_pasted(hash);
                self.status = format!("Copied commit hash {short}{}", landing.note());
            }
            CommitAction::CopyMessage => {
                self.status = match crate::git::commit_message(&root, hash) {
                    Ok(message) => {
                        let landing = self.copy_where_pasted(&message);
                        format!("Copied the message of {short}{}", landing.note())
                    }
                    Err(e) => format!("Could not read the message of {short}: {e}"),
                };
            }
        }
    }

    /// Git: Copy Permalink / Copy Head Link / Open on Remote (#1610): the
    /// active file's selected lines, else the caret's line, on its remote's
    /// web page. The status line says the link, or why there is none.
    pub(super) fn share_link(&mut self, how: ShareLink) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file to link to its lines");
            return;
        };
        let rev = match how {
            ShareLink::CopyHeadLink => crate::git::LinkRev::Branch,
            ShareLink::CopyPermalink | ShareLink::OpenOnRemote => crate::git::LinkRev::Commit,
        };
        let url = match crate::git::share_link(&path, Some(self.share_link_lines()), rev) {
            Ok(url) => url,
            Err(why) => {
                self.status = format!("No link: {why}");
                return;
            }
        };
        if how == ShareLink::OpenOnRemote {
            self.open_url_for_user(&url);
            return;
        }
        // The clipboard the user pastes from, as any copy reaches it: the
        // local one over the relay, else this machine's, else OSC 52.
        let landing = self.copy_where_pasted(&url);
        self.status = format!("Copied {url}{}", landing.note());
    }

    /// The 1-based lines a shared link points at: the selection's, less a
    /// last line it only reaches the start of (whole lines selected with
    /// Shift+Down), else the caret's.
    pub(super) fn share_link_lines(&self) -> (usize, usize) {
        let caret = self.editor.cursor_row + 1;
        let Some(sel) = self.editor.selection.filter(|s| s.has_area()) else {
            return (caret, caret);
        };
        let ((start, _), (end, end_col)) = sel.normalised();
        let end = if end_col == 0 && end > start {
            end - 1
        } else {
            end
        };
        (start + 1, end + 1)
    }

    /// The encoding `path`'s open tab decodes it with, in any group; UTF-8
    /// when it isn't open, which is what opening it would pick: a BOM still
    /// wins in `decode_blob`, and bytes that are not UTF-8 get the plain
    /// open rather than an editable diff of replacement characters.
    pub(super) fn open_tab_encoding(&self, path: &Path) -> &'static encoding_rs::Encoding {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .find(|e| e.diff.is_none() && e.path.as_deref() == Some(path))
            .map_or(encoding_rs::UTF_8, |e| e.encoding)
    }
}
