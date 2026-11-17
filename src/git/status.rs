//! The repository's status: branch, ahead and behind, whether anything changed, ignored
//! paths, the commit message a merge or squash prepared, and the repositories one folder
//! down.

use super::*;

/// Snapshot of the workspace's git state, refreshed periodically and after
/// filesystem events.  Cheap to compute on small repos; we shell out rather
/// than link `git2` to keep the dep graph small and the surface area testable.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct GitStatus {
    pub in_repo: bool,
    /// `Some("main")` on a normal branch.  `None` when the workspace is not
    /// a git repo, or HEAD is detached (use `detached_hash` then).
    pub branch: Option<String>,
    pub detached_hash: Option<String>,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    /// The HEAD commit's full oid, or `None` outside a repo. Drives git-gutter
    /// baseline invalidation: when this moves (commit, checkout, pull) the
    /// cached HEAD file snapshots are stale and must be refetched.
    pub head_oid: Option<String>,
    /// Absolute paths of git-ignored files and directories, so the Explorer
    /// can grey them out (VS Code's ignored-resource decoration). A fully
    /// ignored directory appears as one collapsed entry — descendants are
    /// resolved by walking ancestors, never enumerated. Arc: the status is
    /// cloned per consumer each refresh and the set can be large.
    pub ignored: Arc<HashSet<PathBuf>>,
    /// The Explorer's git decorations (#1513): each changed file under the
    /// workspace root with its status, and each folder above one with the
    /// strongest status beneath it, keyed by absolute path in the root's
    /// own spelling. Arc for the same reason as `ignored`.
    pub decorations: Arc<HashMap<PathBuf, GitDecoration>>,
    /// The repository toplevel containing the workspace root, or `None`
    /// outside a repo. Porcelain and numstat paths are TOPLEVEL-relative,
    /// so every consumer that joins a repo-relative path or names one on a
    /// git command line must resolve against this, never the workspace
    /// root — the two coincide only when the workspace root IS the
    /// toplevel (#139).
    pub repo_root: Option<PathBuf>,
    /// Number of changed entries `git status --porcelain` reported —
    /// available for EVERY workspace folder's worker, unlike the panel's
    /// entry list which only the active repo fetches (#161: the
    /// repositories overview and the cross-repo badge read this).
    pub changed_count: usize,
    /// The message git prepared for the commit in progress (#1282): a
    /// conflicted merge, cherry-pick or revert writes `MERGE_MSG`, a
    /// `merge --squash` writes `SQUASH_MSG`. Comment lines are dropped, as
    /// `commit.cleanup=strip` would. Without either, the `commit.template`
    /// file (#1620). `None` when there is none of them.
    pub prepared_message: Option<String>,
    /// The merge, rebase, cherry-pick, revert or bisect the repository is
    /// in the middle of (#1356), from [`repo_operation`].
    pub operation: Option<RepoOp>,
    /// The branch a rebase is rewriting (#1647). HEAD is detached on the
    /// commit being replayed onto throughout, so `branch` is `None`; git
    /// keeps the branch's name in the rebase's `head-name` file. `None`
    /// outside a rebase, and for a rebase of a detached HEAD.
    pub rebasing_branch: Option<String>,
}

impl GitStatus {
    /// What HEAD is called in the status bar and the Source Control header:
    /// the branch, the branch a rebase is rewriting (#1647), else the short
    /// hash of a detached HEAD.
    pub fn head_label(&self) -> &str {
        match (&self.branch, &self.rebasing_branch, &self.detached_hash) {
            (Some(b), _, _) | (None, Some(b), _) => b,
            (None, None, Some(h)) => h,
            (None, None, None) => "(no head)",
        }
    }
}

pub fn query(root: &Path) -> GitStatus {
    // One probe answers both questions is_git_repo asked: exit status is
    // repo membership, stdout is the toplevel every repo-relative path
    // must resolve against.
    let Some(repo_root) = repo_toplevel(root) else {
        return GitStatus::default();
    };
    let branch = run_git(root, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(parse_branch);
    let detached_hash = if branch.is_none() {
        run_git(root, &["rev-parse", "--short", "HEAD"])
            .ok()
            .and_then(parse_branch)
    } else {
        None
    };
    // Raw `-z` bytes: `run_git` trims stdout, which would shift the first
    // record's status column (` M a.py` -> `M a.py`), and a lossy string
    // would not key a non-UTF-8 filename to its real path.
    let porcelain = git_raw(
        root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
        None,
    )
    .unwrap_or_default();
    let prefix = run_git(root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let decorations = parse_decorations(root, &prefix, &porcelain);
    let changed_count = porcelain_z_count(&porcelain);
    let dirty = changed_count > 0;
    let (ahead, behind) = match run_git(
        root,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    ) {
        Ok(s) => parse_ahead_behind(&s),
        Err(_) => (0, 0),
    };
    let head_oid = run_git(root, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let prepared = prepared_message(root, &repo_root);

    let operation = repo_operation(root);
    GitStatus {
        in_repo: true,
        detached_hash,
        dirty,
        ahead,
        behind,
        head_oid,
        ignored: Arc::new(query_ignored(root)),
        decorations: Arc::new(decorations),
        repo_root: Some(repo_root),
        changed_count,
        prepared_message: prepared,
        rebasing_branch: match (&branch, operation) {
            (None, Some(RepoOp::Rebase { .. })) => rebasing_branch(root),
            _ => None,
        },
        branch,
        operation,
    }
}

/// git's prepared message for the commit in progress (#1282): `MERGE_MSG`
/// (a conflicted merge, cherry-pick or revert), else `SQUASH_MSG`, found
/// through `--git-path` so a worktree or submodule reads its own; else the
/// `commit.template` file (#1620). Comment lines are dropped as
/// `commit.cleanup=strip` drops them, by git's own [`comment_marker`];
/// nothing left is no message.
pub(super) fn prepared_message(root: &Path, top: &Path) -> Option<String> {
    let paths = run_git(
        root,
        &[
            "rev-parse",
            "--git-path",
            "MERGE_MSG",
            "--git-path",
            "SQUASH_MSG",
        ],
    )
    .ok()?;
    // Asked only once there is text to strip: most refreshes have none.
    let marker = std::cell::OnceCell::new();
    let strip = |text: String| {
        without_comments(
            &text,
            marker.get_or_init(|| comment_marker(root)).as_deref(),
        )
    };
    paths
        .lines()
        .find_map(|p| strip(std::fs::read_to_string(root.join(p.trim())).ok()?))
        .or_else(|| strip(commit_template(root, top)?))
}

/// `text` without the lines starting with `marker` and without outer
/// blank lines, or `None` when nothing is left. No marker drops no line.
pub(super) fn without_comments(text: &str, marker: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !marker.is_some_and(|m| l.starts_with(m)))
        .map(str::trim_end)
        .collect();
    let message = kept.join("\n").trim().to_string();
    (!message.is_empty()).then_some(message)
}

/// The marker that starts a comment line for `git commit` (#1620):
/// whichever of `core.commentChar` and `core.commentString` the config
/// sets last, as git reads them, else `#`. A team that writes `#123` in
/// its messages sets another, and its template's help lines start with
/// that one. `None` for `auto`: git then picks a marker no line of the
/// message starts with, so none of it is a comment.
pub(super) fn comment_marker(root: &Path) -> Option<String> {
    let set = run_git(
        root,
        &["config", "--get-regexp", r"^core\.comment(char|string)$"],
    )
    .unwrap_or_default();
    match set.lines().last().and_then(|l| l.split_once(' ')) {
        Some((_, v)) if v.eq_ignore_ascii_case("auto") => None,
        Some((_, v)) => Some(v.to_string()),
        None => Some(String::from("#")),
    }
}

/// The text of the file git's `commit.template` names (#1620), for the
/// message box when no merge or squash message is prepared: croft commits
/// with `-m`, and with `-m` git skips the template, so a team's required
/// ticket prefix and headings were lost. `--path` expands `~`; a relative
/// path is taken from `top`, the repository's top level, where `git
/// commit` reads it.
pub(super) fn commit_template(root: &Path, top: &Path) -> Option<String> {
    let named = run_git(root, &["config", "--path", "commit.template"]).ok()?;
    let named = Path::new(named.trim());
    if named.as_os_str().is_empty() {
        return None;
    }
    std::fs::read_to_string(top.join(named)).ok()
}

/// The git-ignored paths under `root`, absolute.
///
/// Two-step, because neither step alone is correct. `ls-files --directory`
/// gives a cheap candidate set (a fully-ignored directory collapses to one
/// entry instead of enumerating `target/`'s thousands of files), but it also
/// collapses any ENTIRELY UNTRACKED directory whose contents all happen to be
/// ignored — even when no rule matches the directory itself, so `logs/` shows
/// up merely because `logs/app.log` is ignored. `check-ignore` then answers
/// per path, exactly the per-resource question VS Code asks, and only its
/// verdicts reach the set.
///
/// Both calls go through [`git_raw`]: filenames may carry leading or trailing
/// whitespace (git sorts a leading-space name FIRST, so a blanket `trim()`
/// eats it) or bytes that are not UTF-8, so the `-z` output is split on NUL
/// and converted without lossy decoding.
pub(super) fn query_ignored(root: &Path) -> HashSet<PathBuf> {
    let listed = git_raw(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--directory",
            "--exclude-standard",
        ],
        None,
    )
    .unwrap_or_default();
    let mut stdin = Vec::with_capacity(listed.len());
    for candidate in listed.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        stdin.extend_from_slice(candidate);
        stdin.push(0);
    }
    if stdin.is_empty() {
        return HashSet::new();
    }
    // check-ignore exits 1 when nothing matches: a verdict, not a failure, so
    // `git_raw` deliberately does not gate on the exit status.
    let confirmed =
        git_raw(root, &["check-ignore", "-z", "--stdin"], Some(&stdin)).unwrap_or_default();
    confirmed
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|raw| join_raw(root, raw))
        .collect()
}

/// Whether git ignores `path` under the repository at `root`: one
/// `check-ignore` call, for a single path's verdict (#263: a save of build
/// output or a `.env` does not rerun watched tests). Outside a repository,
/// or when git cannot answer, nothing is ignored.
pub fn is_ignored(root: &Path, path: &Path) -> bool {
    let mut stdin = path.as_os_str().as_encoded_bytes().to_vec();
    stdin.push(0);
    git_raw(root, &["check-ignore", "-z", "--stdin"], Some(&stdin))
        .is_some_and(|out| out.iter().any(|b| *b != 0))
}

/// The immediate subfolders of `dir` that hold a repository (a `.git`
/// entry: a folder, or a file for worktrees and submodules), sorted by
/// name. Hidden folders are skipped, like VS Code's depth-1 scan (#1227).
pub fn child_repos(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    repos.sort();
    repos
}

/// Trim whitespace and treat empty as "no branch".
pub fn parse_branch(out: String) -> Option<String> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `git rev-list --left-right --count HEAD...@{u}` returns "<ahead>\t<behind>".
pub fn parse_ahead_behind(out: &str) -> (usize, usize) {
    let mut parts = out.split_whitespace();
    let a = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (a, b)
}
