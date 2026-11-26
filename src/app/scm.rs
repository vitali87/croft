//! Source Control: the panel and its keys, background git and its log, commits,
//! push, pull and sync, stash, branches and pickers, hunks, permalinks, the commit
//! graph, its dialogs, change gutters and inline blame.

use super::*;
use crate::widgets::confirm_frame::{ConfirmFrame, Inset, draw_confirm, yes_no_keys};
use crate::widgets::picker_rows::{PickerMouse, answer_mouse};
use crate::widgets::source_control::{CardButton, KeyStop};

/// The *Git: … Changes* commands that act on one file (#871).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitFileCommand {
    Open,
    Stage,
    Unstage,
    Discard,
}

/// A *Git: … Changes* command run on the open file, waiting on its own
/// read of the repository's changes (#871): Source Control's list is only
/// kept fresh while the view is open, so the command never trusts it.
pub(super) struct GitFileLookup {
    command: GitFileCommand,
    /// The open file, as the editor names it.
    path: PathBuf,
    /// The workspace folder whose repository was read: the reply is
    /// dropped when Source Control has moved to another one meanwhile.
    folder: PathBuf,
    job: crate::jobs::Job<(crate::git::GitStatus, Vec<crate::git::ChangeEntry>)>,
}

impl GitFileCommand {
    /// The change list's own key for it, which is its shortcut.
    fn key(self) -> KeyCode {
        match self {
            GitFileCommand::Open => KeyCode::Enter,
            GitFileCommand::Stage => KeyCode::Char('s'),
            GitFileCommand::Unstage => KeyCode::Char('u'),
            GitFileCommand::Discard => KeyCode::Char('d'),
        }
    }
}

/// Source Control's `⋯` menu laid out (#871): its rows and top column, and
/// the open fly-out's rows and column.
pub(super) struct ScmMenuLayout {
    /// Which plain panel the top column is.
    pub(super) panel: crate::widgets::scm_menu::Panel,
    pub(super) rows: Vec<crate::widgets::scm_menu::Row>,
    pub(super) top: crate::widgets::scm_menu::Column,
    pub(super) fly_out: Option<(
        Vec<crate::widgets::scm_menu::Row>,
        crate::widgets::scm_menu::Column,
    )>,
}

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
                for group in self.editor_layout.all_groups_mut(&mut self.editor) {
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
        let worker = self.git_worker_for_root(&active);
        let (status, known) = (worker.status().clone(), worker.reported());
        self.source_control.status_known = known;
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
        let rows = if self.roots.is_multi() {
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
        self.source_control.set_repositories(rows);
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
        let groups = self.editor_layout.all_groups_mut(&mut self.editor);
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
        let title = format!(" {} ", crate::i18n::tr("DISCARD CHANGES?"));
        let frame_spec = ConfirmFrame {
            title: &title,
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 8,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
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
                yes_no_keys("es, discard"),
            ]);
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
    }

    /// Confirmation modal for Revert Hunk: destroys the uncommitted edit
    /// under the diff caret, so it gets the same red Y/N gate as discard.
    pub(super) fn render_revert_hunk_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pr) = self.pending_revert_hunk.as_ref() else {
            return;
        };
        let frame_spec = ConfirmFrame {
            title: " REVERT HUNK? ",
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 8,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
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
                yes_no_keys("es, revert"),
            ]);
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
    }

    /// The unsaved-changes prompt in front of a commit or stash (#1438):
    /// names the unsaved files and offers Save All & Commit, Commit Anyway
    /// or Cancel, as VS Code does.
    pub(super) fn render_unsaved_scm_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pending) = self.pending_unsaved_scm.as_ref() else {
            return;
        };
        let accent = self.theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b));
        let title = format!(" {} ", crate::i18n::tr("UNSAVED CHANGES"));
        let frame_spec = ConfirmFrame {
            title: &title,
            accent,
            title_fg: Color::Black,
            height: 8,
            inset: Inset::Wide,
        };
        let verb = pending.op.verb();
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
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
        });
    }

    /// Confirmation modal for "Discard All Changes" — reverts every tracked
    /// edit to HEAD, so it gets the same red Y/N gate as a single discard.
    pub(super) fn render_discard_all_confirm(&self, frame: &mut ratatui::Frame) {
        if !self.pending_discard_all {
            return;
        }
        let frame_spec = ConfirmFrame {
            title: " DISCARD ALL CHANGES? ",
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 7,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let body = ratatui::text::Text::from(vec![
                ratatui::text::Line::from(ratatui::text::Span::styled(
                    "This reverts every tracked file to HEAD. Untracked files are kept.",
                    Style::default().fg(self.theme.ui(Color::White)),
                )),
                ratatui::text::Line::from(""),
                yes_no_keys("es, discard all"),
            ]);
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
    }

    /// Confirmation modal for Drop Stash (#1128), in the Discard All style.
    /// Only Y drops: the pick that opened it was an Enter, and a second
    /// stray one must not delete the stash.
    pub(super) fn render_stash_drop_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(pending) = &self.pending_stash_drop else {
            return;
        };
        let frame_spec = ConfirmFrame {
            title: " DROP STASH? ",
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 8,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
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
                yes_no_keys("es, drop it"),
            ]);
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
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
        self.source_control.settle_keys();
        match self.source_control.keys {
            KeyStop::Repositories => return self.handle_repositories_key(key),
            KeyStop::Commits => return self.handle_commit_graph_key(key),
            KeyStop::List => return self.handle_source_control_list_key(key),
            KeyStop::Card => return self.handle_card_key(key),
            KeyStop::Message => {}
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
            // Tab steps to the change list, or past an empty one to COMMITS
            // (#871), so a clean repository still reaches its history;
            // Shift+Tab goes back up to REPOSITORIES, or round to COMMITS.
            KeyCode::Tab => self.step_source_control_keys(true),
            KeyCode::BackTab => self.step_source_control_keys(false),
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
            // Source Control's Tab stops, in VS Code's order (#871): the
            // message box, the change list, COMMITS, and round again.
            (KeyCode::BackTab, _) => self.step_source_control_keys(false),
            (KeyCode::Tab, _) => self.step_source_control_keys(true),
            (KeyCode::Char('r'), _) => self.refresh_source_control_from_key(),
            (KeyCode::Up, _) => {
                if !sc.move_selection(true) {
                    sc.keys = KeyStop::Message;
                }
            }
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

    /// *Git: Open Changes*, *Stage Changes*, *Unstage Changes* and *Discard
    /// Changes* (#871). While Source Control's change list has the keys they
    /// do what its own `Enter`, `S`, `U` and `D` do to the selected row,
    /// which are their shortcuts; anywhere else they act on the open file
    /// (a diff tab's file), as VS Code's do, once a fresh read of the
    /// repository's changes lands ([`Self::drain_git_file_lookup`]). A file
    /// with a row in each section gives Unstage its Staged row and the
    /// others its Changes row.
    pub(super) fn git_file_command(&mut self, command: GitFileCommand) {
        if self.focus == Pane::Tree
            && self.sidebar_view == SidebarView::SourceControl
            && self.source_control.keys == KeyStop::List
        {
            let key = KeyEvent::new(command.key(), KeyModifiers::NONE);
            self.handle_source_control_list_key(key);
            return;
        }
        let Some(path) = self.open_file_path() else {
            self.status = String::from("No file is open");
            return;
        };
        let folder = self.active_scm_root.clone();
        // One at a time, in the order they were run: each reads the changes
        // after the one before it has acted, so Stage then Unstage sees
        // what Stage staged. Every command joins the back of the line, and
        // the line starts its head, so a command run while an earlier one
        // waits (behind a discard's question, say) never jumps ahead of it.
        self.git_file_queue.push_back((command, path, folder));
        self.start_next_git_file_command();
    }

    /// Read `folder`'s repository's changes off the UI thread for
    /// `command` on `path`; [`Self::drain_git_file_lookup`] finishes it.
    fn start_git_file_lookup(&mut self, command: GitFileCommand, path: PathBuf, folder: PathBuf) {
        let root = self
            .git_worker_for_root(&folder)
            .status()
            .repo_root
            .clone()
            .unwrap_or_else(|| folder.clone());
        let job = crate::jobs::Job::spawn("git-file-command", crate::jobs::Start::Now, move || {
            (crate::git::query(&root), crate::git::query_changes(&root))
        });
        match job {
            Ok(job) => {
                self.status = String::from("Git: reading changes\u{2026}");
                self.git_file_lookup = Some(GitFileLookup {
                    command,
                    path,
                    folder,
                    job,
                });
            }
            Err(e) => self.status = format!("Could not read the changes: {e}"),
        }
    }

    /// The file the open tab shows: a diff tab's file, the old side of a
    /// deletion (whose new side is `/dev/null`), else the editor's file. A
    /// deletion's view names its file relative to the repository.
    fn open_file_path(&self) -> Option<PathBuf> {
        let diff_file = self.editor.diff.as_ref().map(|d| {
            if d.right_path == Path::new("/dev/null") {
                d.left_path.clone()
            } else {
                d.right_path.clone()
            }
        });
        let path = diff_file.or_else(|| self.editor.path.clone())?;
        Some(if path.is_relative() {
            self.scm_root().join(path)
        } else {
            path
        })
    }

    /// Finish a *Git: … Changes* command once its read of the changes has
    /// landed: show the fresh list and act on the open file's row. True
    /// when something changed on screen.
    pub(super) fn drain_git_file_lookup(&mut self) -> bool {
        // A discard asking first holds every command: one whose read is in
        // flight goes back to the front of the line and reads again once
        // the question is answered, so it neither replaces the file the
        // question is about nor acts behind it on what the read saw before
        // the answer.
        if self.pending_discard.is_some() {
            if let Some(lookup) = self.git_file_lookup.take() {
                self.git_file_queue
                    .push_front((lookup.command, lookup.path, lookup.folder));
            }
            return false;
        }
        let started = self.start_next_git_file_command();
        let reply = match self.git_file_lookup.as_ref().map(|l| l.job.poll()) {
            None | Some(crate::jobs::Poll::Pending) => return started,
            Some(crate::jobs::Poll::Done(reply)) => Some(reply),
            // Its thread crashed, and the crash notice said so.
            Some(crate::jobs::Poll::Died) => None,
        };
        let Some(lookup) = self.git_file_lookup.take() else {
            return started;
        };
        self.finish_git_file_lookup(lookup, reply);
        self.start_next_git_file_command();
        true
    }

    /// Start the next waiting *Git: … Changes* command, unless one is still
    /// reading or a discard is asking first: a command that ran meanwhile
    /// would act behind the question, and a second Discard would replace
    /// the file the open question is about. The queue moves on once the
    /// question is answered either way. True when one started.
    fn start_next_git_file_command(&mut self) -> bool {
        if self.git_file_lookup.is_some() || self.pending_discard.is_some() {
            return false;
        }
        let Some((command, path, folder)) = self.git_file_queue.pop_front() else {
            return false;
        };
        self.start_git_file_lookup(command, path, folder);
        true
    }

    /// Act on the open file's row in the changes `reply` read, against the
    /// repository root that read found: before git's first status lands the
    /// app's own idea of the root is the workspace folder, which for a
    /// folder inside a repository is not where git's paths start.
    fn finish_git_file_lookup(
        &mut self,
        lookup: GitFileLookup,
        reply: Option<(crate::git::GitStatus, Vec<crate::git::ChangeEntry>)>,
    ) {
        let Some((status, entries)) = reply else {
            self.status = String::from("Could not read the changes");
            return;
        };
        if lookup.folder != self.active_scm_root {
            self.status =
                String::from("Git: cancelled: Source Control moved to another repository");
            return;
        }
        let root = status.repo_root.clone().unwrap_or(lookup.folder);
        let Some(rel) = relative_to_repo(&lookup.path, &root) else {
            self.status = format!("{} is outside the repository", lookup.path.display());
            return;
        };
        self.source_control.set_status(status, entries);
        let Some(idx) = self.change_row_for(lookup.command, &rel) else {
            return;
        };
        match lookup.command {
            GitFileCommand::Open => self.open_source_control_entry_in(&root, idx),
            GitFileCommand::Stage => self.stage_source_control_entry_in(&root, idx),
            GitFileCommand::Unstage => self.unstage_source_control_entry_in(&root, idx),
            GitFileCommand::Discard => self.request_discard_source_control_entry_in(&root, idx),
        }
    }

    /// The change list's row for `rel` that `command` acts on, or `None`
    /// with the status line saying why.
    fn change_row_for(&mut self, command: GitFileCommand, rel: &str) -> Option<usize> {
        use crate::git::ChangeSection;
        let rows: Vec<(usize, bool)> = self
            .source_control
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.path == rel)
            .map(|(i, e)| (i, e.kind.section() == ChangeSection::Staged))
            .collect();
        let staged = rows.iter().find(|(_, staged)| *staged).map(|(i, _)| *i);
        let changed = rows.iter().find(|(_, staged)| !*staged).map(|(i, _)| *i);
        let row = match command {
            GitFileCommand::Unstage => staged,
            GitFileCommand::Stage => changed,
            GitFileCommand::Open | GitFileCommand::Discard => changed.or(staged),
        };
        if row.is_none() {
            self.status = match (command, rows.is_empty()) {
                (_, true) => format!("No changes in {rel}"),
                (GitFileCommand::Unstage, false) => format!("No staged changes in {rel}"),
                _ => format!("No unstaged changes in {rel}"),
            };
        }
        row
    }

    /// Tab (`forward`) or Shift+Tab in Source Control (#871): the keys go
    /// to the next of the view's stops on screen, in VS Code's order
    /// (REPOSITORIES, the message box, the change list, COMMITS; or with no
    /// repository, REPOSITORIES and the card's buttons), round again past
    /// either end.
    pub(super) fn step_source_control_keys(&mut self, forward: bool) {
        match self.source_control.next_stop(forward) {
            KeyStop::Repositories => self.source_control.focus_repositories(),
            KeyStop::Message => self.source_control.keys = KeyStop::Message,
            KeyStop::List => {
                self.source_control.focus_list();
            }
            KeyStop::Commits => self.focus_commit_graph(),
            KeyStop::Card => self.source_control.focus_card(forward),
        }
    }

    /// Show the repository of workspace folder `root` in Source Control,
    /// pinned there (#161) until the focus next crosses folders: what a
    /// click on its REPOSITORIES row, or Enter on it, does.
    pub(super) fn pin_scm_repository(&mut self, root: PathBuf) {
        self.scm_pin = Some(root.clone());
        self.seed_active_scm(root);
    }

    /// A key while REPOSITORIES has the keys (#871): the arrows, Home and
    /// End move its keyboard row, Enter or Space shows that row's
    /// repository, R refreshes, and Tab / Shift+Tab go on to the message
    /// box (or the card) / back round to COMMITS.
    pub(super) fn handle_repositories_key(&mut self, key: KeyEvent) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return;
        }
        let sc = &mut self.source_control;
        match key.code {
            KeyCode::Tab => self.step_source_control_keys(true),
            KeyCode::BackTab => self.step_source_control_keys(false),
            KeyCode::Up => sc.move_repo_cursor(-1),
            KeyCode::Down => sc.move_repo_cursor(1),
            KeyCode::Home => sc.repo_cursor = 0,
            KeyCode::End => sc.move_repo_cursor(isize::MAX),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(root) = sc.repo_under_cursor().map(|r| r.workspace_root.clone()) {
                    self.pin_scm_repository(root);
                }
            }
            KeyCode::Char('r' | 'R') => self.refresh_source_control_from_key(),
            _ => {}
        }
    }

    /// A key while the no-repository card has the keys (#871): Tab and
    /// Shift+Tab walk its buttons and then go on round the view, Up and
    /// Down walk the buttons, Enter or Space presses the one with the
    /// keys, and R refreshes.
    pub(super) fn handle_card_key(&mut self, key: KeyEvent) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return;
        }
        let sc = &mut self.source_control;
        match key.code {
            KeyCode::Tab => {
                if !sc.move_card_button(true) {
                    self.step_source_control_keys(true);
                }
            }
            KeyCode::BackTab => {
                if !sc.move_card_button(false) {
                    self.step_source_control_keys(false);
                }
            }
            KeyCode::Up => {
                sc.move_card_button(false);
            }
            KeyCode::Down => {
                sc.move_card_button(true);
            }
            KeyCode::Enter | KeyCode::Char(' ') => match sc.keyed_card_button() {
                Some(CardButton::Initialize) => self.initialize_repository(),
                Some(CardButton::Open(repo)) => self.change_workspace_root(repo),
                None => {}
            },
            KeyCode::Char('r' | 'R') => self.refresh_source_control_from_key(),
            _ => {}
        }
    }

    /// Git: Initialize Repository from the Command Palette (#871): what the
    /// no-repository card's button does, which itself refuses a folder
    /// inside a repository. In one whose subfolders are repositories (where
    /// the card offers only their Open, as `git init` would wrap them), a
    /// line says why not.
    pub(super) fn initialize_repository_command(&mut self) {
        let folder = self.active_scm_root.clone();
        if !crate::git::child_repos(&folder).is_empty() {
            self.status = format!(
                "{} holds repositories in its subfolders: open one from Source Control",
                folder.display()
            );
            return;
        }
        self.initialize_repository();
    }

    /// COMMITS takes the keys (#871), its first row picked when none is.
    pub(super) fn focus_commit_graph(&mut self) {
        self.source_control.keys = KeyStop::Commits;
        if self.commit_graph.selected_commit().is_none() {
            self.commit_graph.select(0);
        }
    }

    /// Git: Refresh (`R` in Source Control's lists, #871): what the header's
    /// refresh button does.
    pub(super) fn refresh_source_control_from_key(&mut self) {
        self.refresh_source_control();
        self.status = String::from("Refreshed Source Control");
    }

    /// A key while COMMITS has the keys (#871): the arrows, Home / End and
    /// PageUp / PageDown pick a commit, Enter opens its changes, Left and
    /// Right collapse and expand the section, R refreshes, and Tab /
    /// Shift+Tab go on to the message box / back to the change list.
    pub(super) fn handle_commit_graph_key(&mut self, key: KeyEvent) {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return;
        }
        let graph = &mut self.commit_graph;
        let open = !graph.collapsed;
        match key.code {
            // On round to REPOSITORIES or the message box; back to the
            // change list, or past an empty one to the message box.
            KeyCode::Tab => self.step_source_control_keys(true),
            KeyCode::BackTab => self.step_source_control_keys(false),
            KeyCode::Up if open => graph.move_selection(-1),
            KeyCode::Down if open => graph.move_selection(1),
            KeyCode::PageUp if open => {
                let page = graph.page() as isize;
                graph.move_selection(-page);
            }
            KeyCode::PageDown if open => {
                let page = graph.page() as isize;
                graph.move_selection(page);
            }
            KeyCode::Home if open => graph.select(0),
            KeyCode::End if open => {
                let last = graph.last_row();
                graph.select(last);
            }
            KeyCode::Left if open => graph.toggle_collapse(),
            KeyCode::Right if !open => graph.toggle_collapse(),
            KeyCode::Enter if open => {
                if let Some((hash, short)) = graph
                    .selected_commit()
                    .map(|c| (c.hash.clone(), c.short_hash.clone()))
                {
                    self.open_commit_patch(&hash, &short);
                }
            }
            KeyCode::Char('r' | 'R') => self.refresh_source_control_from_key(),
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
        let root = self.scm_root();
        self.open_source_control_entry_in(&root, entry_idx);
    }

    /// [`Self::open_source_control_entry`] for the repository at
    /// `scm_root`.
    pub(super) fn open_source_control_entry_in(&mut self, scm_root: &Path, entry_idx: usize) {
        use crate::git::ChangeKind;
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        self.source_control.selected_change = Some(entry_idx);
        // Porcelain paths are toplevel-relative (#139): join and HEAD-read
        // against the repo root, never the workspace root.
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
                if crate::git::is_submodule(scm_root, &entry.path) =>
            {
                match crate::git::submodule_diff(scm_root, &entry.path) {
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
                    crate::git::read_bytes_at_head(scm_root, &entry.path).and_then(|head| {
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
                match crate::git::read_file_at_head(scm_root, &entry.path) {
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

    /// The card's Initialize Repository: `git init` in the folder Source
    /// Control shows, which with several folders open is the focused
    /// file's, not the first (#2734).
    pub(super) fn initialize_repository(&mut self) {
        let folder = self.active_scm_root.clone();
        // Asked of git itself, right before acting: Source Control's own
        // status may not have arrived yet, or be the last folder's, and
        // `git init` inside a repository nests a new one in it (#871).
        let inside = crate::proc::command("git")
            .arg("-C")
            .arg(&folder)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|out| out.status.success());
        if inside {
            self.status = format!("{} is already in a Git repository", folder.display());
            return;
        }
        match crate::proc::command("git")
            .arg("-C")
            .arg(&folder)
            .arg("init")
            .output()
        {
            Ok(out) if out.status.success() => {
                self.status = format!("Initialized empty Git repository in {}", folder.display());
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
        let root = self.scm_root();
        self.unstage_source_control_entry_in(&root, entry_idx);
    }

    /// [`Self::unstage_source_control_entry`] in the repository at `root`.
    pub(super) fn unstage_source_control_entry_in(&mut self, root: &Path, entry_idx: usize) {
        if self.repo_git_busy() {
            return;
        }
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        match crate::git::unstage_path(root, &entry.path) {
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
        let groups = self.editor_layout.all_groups(&self.editor);
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
        for group in self.editor_layout.all_groups_mut(&mut self.editor) {
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
        self.close_scm_menus();
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
        let root = self.scm_root();
        self.stage_source_control_entry_in(&root, entry_idx);
    }

    /// [`Self::stage_source_control_entry`] in the repository at `root`.
    pub(super) fn stage_source_control_entry_in(&mut self, root: &Path, entry_idx: usize) {
        if self.repo_git_busy() {
            return;
        }
        let Some(entry) = self.source_control.entries.get(entry_idx).cloned() else {
            return;
        };
        match crate::git::stage_path(root, &entry.path) {
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
        relative_to_repo(path, &self.scm_root())
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
        let root = self.scm_root();
        self.request_discard_source_control_entry_in(&root, entry_idx);
    }

    /// [`Self::request_discard_source_control_entry`] in the repository at
    /// `root`, which the confirmed discard then acts in.
    pub(super) fn request_discard_source_control_entry_in(
        &mut self,
        root: &Path,
        entry_idx: usize,
    ) {
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
            root: root.to_path_buf(),
            rel_path: entry.path,
            untracked,
            staged,
        });
    }

    pub fn confirm_pending_discard(&mut self) {
        let Some(pd) = self.pending_discard.take() else {
            return;
        };
        match crate::git::discard_path(&pd.root, &pd.rel_path, pd.untracked, pd.staged) {
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
        self.close_scm_menus();
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
        match answer_mouse(picker, m) {
            PickerMouse::Close => self.close_branch_picker(),
            PickerMouse::Accept => self.apply_branch_picker_selection(),
            PickerMouse::Stay => {}
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

    /// Source Control's `⋯` actions menu (#871), opened on the shared menu
    /// with its own look. A click opens it with no row highlighted, as it
    /// always has; the keyboard (`keyed`) opens it with its first row
    /// highlighted. The operation the repository is in leads it (#1356).
    pub(super) fn open_scm_actions_menu(&mut self, keyed: bool) {
        let op = crate::git::repo_operation(&self.scm_root());
        self.open_scm_actions_menu_for(op, keyed);
    }

    /// Open Source Control's `⋯` menu for a repository in `op`, whose rows
    /// lead it (#1356).
    pub(super) fn open_scm_actions_menu_for(
        &mut self,
        op: Option<crate::git::RepoOp>,
        keyed: bool,
    ) {
        use crate::widgets::scm_menu::{Node, menu_for};
        let leaf = |l: &crate::widgets::scm_menu::Leaf| {
            MenuEntry::item(l.label, MenuAction::Scm(l.action))
        };
        let items: Vec<MenuEntry> = menu_for(op)
            .iter()
            .map(|node| match node {
                Node::Item(l) => leaf(l),
                Node::Sep => MenuEntry::Separator,
                Node::Sub { label, items, .. } => MenuEntry::Submenu {
                    label: label.to_string(),
                    items: items.iter().map(leaf).collect(),
                },
            })
            .collect();
        let selected = items.iter().position(MenuEntry::is_selectable).unwrap_or(0);
        self.context_menu = Some(ContextMenu {
            origin: (0, 0),
            items,
            selected,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.scm_root(),
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            look: MenuLook::ScmActions { op },
            keyed,
        });
    }

    /// Close Source Control's `⋯` menu or Commit dropdown, when one is the
    /// menu open: a prompt or picker one of its rows opened takes the keys
    /// from here on, and neither outlives its view.
    pub(super) fn close_scm_menus(&mut self) {
        if self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.look != MenuLook::Panel)
        {
            self.context_menu = None;
        }
    }

    /// The Commit button's `▾` dropdown (#871), opened on the shared menu
    /// with Source Control's look, under the `▾`. A click opens it with no
    /// row highlighted; the keyboard (`keyed`) with its first row
    /// highlighted.
    pub(super) fn open_commit_actions_menu(&mut self, keyed: bool) {
        use crate::widgets::source_control::CommitMenuItem;
        let default_branch = self.default_branch_label.as_deref().unwrap_or("default");
        let items = CommitMenuItem::ALL
            .iter()
            .map(|item| MenuEntry::item(item.label(default_branch), MenuAction::Commit(*item)))
            .collect();
        self.context_menu = Some(ContextMenu {
            origin: (0, 0),
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.scm_root(),
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            look: MenuLook::CommitActions,
            keyed,
        });
    }

    /// Git: Show Commit Actions (Cmd+K Shift+C, #871): the Commit button's
    /// dropdown from the keyboard, showing Source Control first when it is
    /// not up.
    pub(super) fn open_commit_actions_menu_from_keyboard(&mut self) {
        if self.sidebar_view != SidebarView::SourceControl || !self.show_tree {
            self.set_sidebar_view(SidebarView::SourceControl);
        }
        self.open_commit_actions_menu(true);
    }

    /// A left click while the Commit dropdown is open, answered as the
    /// dropdown always answered it: a row runs its action, and a click on
    /// the `▾` closes it; both are spent here (`true`). Anywhere else closes
    /// the dropdown and the click goes on to what it lands on (`false`).
    pub(super) fn click_commit_actions_menu(&mut self, x: u16, y: u16) -> bool {
        let row = self.scm_menu_layout().and_then(|l| l.top.row_at(x, y));
        let Some(menu) = self.context_menu.take() else {
            return false;
        };
        if let Some(action) = row
            .and_then(|i| menu.items.get(i))
            .and_then(MenuEntry::action)
            .cloned()
        {
            self.dispatch_menu_action(action, menu.target_dir);
            return true;
        }
        self.source_control.click_commit_caret(x, y)
    }

    /// A left click while Source Control's `⋯` menu is open (#871), answered
    /// as that menu always answered it: a row of the open fly-out runs its
    /// action; a fly-out's row in the top column opens that fly-out, or
    /// closes it when it is the one open; an action's row runs it; anywhere
    /// else, a separator included, closes the menu.
    pub(super) fn click_scm_actions_menu(&mut self, x: u16, y: u16) {
        let Some(layout) = self.scm_menu_layout() else {
            self.context_menu = None;
            return;
        };
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        let fly_out_hit = layout
            .fly_out
            .as_ref()
            .and_then(|(_, col)| col.row_at(x, y))
            .zip(menu.open_submenu);
        let action = if let Some((child, parent)) = fly_out_hit {
            match menu.items.get(parent) {
                Some(MenuEntry::Submenu { items, .. }) => {
                    items.get(child).and_then(MenuEntry::action).cloned()
                }
                _ => None,
            }
        } else if let Some(row) = layout.top.row_at(x, y) {
            match menu.items.get(row) {
                Some(MenuEntry::Item { action, .. }) => Some(action.clone()),
                Some(MenuEntry::Submenu { .. }) => {
                    let was_open = menu.open_submenu == Some(row);
                    menu.close_submenu();
                    if !was_open {
                        menu.selected = row;
                        menu.open_cursor_submenu();
                    }
                    return;
                }
                _ => None,
            }
        } else {
            None
        };
        let dir = menu.target_dir.clone();
        self.context_menu = None;
        if let Some(action) = action {
            self.dispatch_menu_action(action, dir);
        }
    }

    /// Git: Show Source Control Actions (Cmd+K Shift+A, #871): the `⋯` menu
    /// from the keyboard, showing Source Control first when it is not up.
    /// The view already up keeps its focus, in the message box or the list.
    pub(super) fn open_scm_actions_menu_from_keyboard(&mut self) {
        if self.sidebar_view != SidebarView::SourceControl || !self.show_tree {
            self.set_sidebar_view(SidebarView::SourceControl);
        }
        self.open_scm_actions_menu(true);
    }

    /// Source Control's open menu as it is laid out now: the `⋯` menu's top
    /// column under the header's `⋯` pill with its open fly-out beside its
    /// row, or the Commit dropdown under the button's `▾`. `None` unless one
    /// of them is open and what it hangs from is drawn.
    pub(super) fn scm_menu_layout(&self) -> Option<ScmMenuLayout> {
        use crate::widgets::scm_menu;
        let menu = self.context_menu.as_ref()?;
        let card = crate::widgets::surface::tones(self.theme).is_some();
        let op = match menu.look {
            MenuLook::Panel => return None,
            MenuLook::CommitActions => {
                let default_branch = self.default_branch_label.as_deref().unwrap_or("default");
                let rows = scm_menu::commit_rows(default_branch);
                let top = scm_menu::commit_column(
                    &rows,
                    self.source_control.last_commit_caret_area,
                    self.source_control.last_button_area,
                    self.last_frame_area,
                    card,
                )?;
                let top = top.shown_from(menu.scroll_for(top.shown()));
                return Some(ScmMenuLayout {
                    panel: scm_menu::Panel::Commit,
                    rows,
                    top,
                    fly_out: None,
                });
            }
            MenuLook::ScmActions { op } => op,
        };
        let anchor = self.source_control.header_more_btn;
        if anchor.width == 0 {
            return None;
        }
        let screen = self.last_frame_area;
        let nodes = scm_menu::menu_for(op);
        let rows = scm_menu::rows(&nodes);
        // On a terminal too short for every row, each column shows a window
        // of them kept around the keyboard's row, as croft's other menus do.
        let top = scm_menu::top_column(&rows, anchor, screen, card);
        let top = top.shown_from(menu.scroll_for(top.shown()));
        let fly_out = menu.open_submenu.and_then(|parent| {
            let items = scm_menu::fly_out_rows(&nodes, parent)?;
            let col = scm_menu::fly_out_column(top, parent, &items, screen, card);
            let col = col.shown_from(menu.submenu_scroll_for(col.shown()));
            Some((items, col))
        });
        Some(ScmMenuLayout {
            panel: scm_menu::Panel::Top,
            rows,
            top,
            fly_out,
        })
    }

    /// Paint Source Control's open menu: the top column, then the open
    /// fly-out. The keyboard's row shows once the keyboard drives the menu,
    /// on the level it is on; a card lifts the row under the pointer too.
    pub(super) fn render_scm_menu(&self, frame: &mut ratatui::Frame) {
        use crate::widgets::scm_menu::{Marks, Panel, paint_column};
        let (Some(menu), Some(layout)) = (self.context_menu.as_ref(), self.scm_menu_layout())
        else {
            return;
        };
        let cursor_colours = if self.popup_gradient() {
            (rgb_color(POPUP_SEL_BG), Color::White)
        } else {
            (self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)), Color::Black)
        };
        let in_fly_out = menu.open_submenu.is_some();
        let top_marks = Marks {
            open: menu.open_submenu,
            cursor: (menu.keyed && !in_fly_out).then_some(menu.selected),
            pointer: self.pointer_cell,
            cursor_colours,
        };
        let buf = frame.buffer_mut();
        paint_column(
            buf,
            layout.top,
            &layout.rows,
            layout.panel,
            top_marks,
            self.theme,
        );
        if let Some((items, col)) = &layout.fly_out {
            let marks = Marks {
                open: None,
                cursor: (menu.keyed && in_fly_out).then_some(menu.submenu_selected),
                pointer: self.pointer_cell,
                cursor_colours,
            };
            paint_column(buf, *col, items, Panel::FlyOut, marks, self.theme);
        }
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
            look: MenuLook::Panel,
            keyed: true,
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
        self.editor_layout
            .all_groups(&self.editor)
            .flat_map(|g| g.editors.iter())
            .find(|e| e.diff.is_none() && e.path.as_deref() == Some(path))
            .map_or(encoding_rs::UTF_8, |e| e.encoding)
    }
}

/// `path` as git names it in the repository at `root`: relative, with `/`
/// between parts. A workspace opened through a symlink (macOS's `/var` is
/// `/private/var`) names the file one way and git's root the other, so a
/// path outside `root` as spelled is retried with both resolved.
pub(super) fn relative_to_repo(path: &Path, root: &Path) -> Option<String> {
    let rel = match path.strip_prefix(root) {
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
