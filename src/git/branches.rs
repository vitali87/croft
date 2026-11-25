//! Branches: the default branch, local and remote branches listed, and switching to,
//! tracking, creating, renaming, deleting, merging and rebasing them.

use super::*;

/// The branch HEAD is on, or `None` when detached.
///
/// `--quiet` so a detached HEAD is an empty result rather than an error the
/// caller has to distinguish from a failure.
pub fn current_branch(root: &Path) -> Option<String> {
    let out = run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Repo's default branch name. Tries `origin/HEAD` first (set by
/// `git clone` / `git remote set-head -a origin`), then falls back to
/// common local names (`main`, `master`, `develop`, `trunk`) by checking
/// `git rev-parse --verify <name>`. Returns `Err` when none resolves so
/// the caller can surface the precise failure instead of guessing.
pub fn default_branch(root: &Path) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    if let Ok(out) = crate::proc::command("git")
        .args([
            "-C",
            path_str,
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        && out.status.success()
    {
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(short) = raw.strip_prefix("origin/") {
            if !short.is_empty() {
                return Ok(short.to_string());
            }
        } else if !raw.is_empty() {
            return Ok(raw);
        }
    }
    for candidate in ["main", "master", "develop", "trunk"] {
        let out = crate::proc::command("git")
            .args([
                "-C",
                path_str,
                "rev-parse",
                "--verify",
                "--quiet",
                candidate,
            ])
            .output();
        if let Ok(o) = out
            && o.status.success()
        {
            return Ok(candidate.to_string());
        }
    }
    Err(
        "Could not resolve a default branch (tried origin/HEAD, main, master, develop, trunk)"
            .to_string(),
    )
}

/// A single branch the Checkout/Create picker can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// What the user sees: the local branch name, or `origin/foo` for a
    /// remote-tracking branch.
    pub display: String,
    /// The branch's own name: the local name, or a remote branch's name
    /// without its remote (`feat` for `upstream/feat`). What merge, rebase,
    /// delete and create-from act on.
    pub checkout_name: String,
    /// True for the branch currently checked out (marked in the picker,
    /// never offered as a checkout target).
    pub is_current: bool,
    /// True for `origin/…` remote-tracking branches, listed below locals.
    pub is_remote: bool,
    /// For a remote-tracking branch, the ref to check out with `git switch
    /// --track` (`upstream/feat`), so the local branch tracks the remote
    /// the user picked. `git switch feat` refuses a name two remotes carry
    /// (#1445). `None` for a local branch.
    pub track: Option<String>,
}

/// List branches for the Checkout picker: local branches first (most
/// recently committed on top, the current one flagged), then remote-only
/// branches whose name has no local counterpart. Remote rows carry their
/// remote ref in `track`, so selecting `upstream/foo` runs `git switch
/// --track upstream/foo` and the new local branch tracks that remote.
pub fn list_branches(root: &Path) -> Result<Vec<BranchInfo>, String> {
    let locals_raw = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(HEAD)",
            "refs/heads",
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut branches: Vec<BranchInfo> = Vec::new();
    let mut local_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in locals_raw.lines() {
        let mut parts = line.split('\t');
        let Some(name) = parts.next() else { continue };
        if name.is_empty() {
            continue;
        }
        let is_current = parts.next().map(|m| m.trim() == "*").unwrap_or(false);
        local_names.insert(name.to_string());
        branches.push(BranchInfo {
            display: name.to_string(),
            checkout_name: name.to_string(),
            is_current,
            is_remote: false,
            track: None,
        });
    }
    let remotes_raw = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(symref)",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    // Longest first, so `my/fork/x` splits at the `my/fork` remote and not
    // at a `my` one; splitting at the first `/` broke a remote named with
    // one (#1445).
    let mut remotes: Vec<String> = run_git(root, &["remote"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    remotes.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for line in remotes_raw.lines() {
        let (refname, symref) = line.split_once('\t').unwrap_or((line, ""));
        // `refs/remotes/origin/HEAD` is a pointer to a branch, not one: git
        // shortens it to plain `origin`, which the old `/HEAD` check missed.
        let Some(full) = refname.strip_prefix("refs/remotes/") else {
            continue;
        };
        if full.is_empty() || !symref.is_empty() || full.ends_with("/HEAD") {
            continue;
        }
        let short = remotes
            .iter()
            .find_map(|r| full.strip_prefix(r.as_str())?.strip_prefix('/'))
            .or_else(|| full.split_once('/').map(|(_, s)| s))
            .unwrap_or(full);
        if local_names.contains(short) {
            continue;
        }
        branches.push(BranchInfo {
            display: full.to_string(),
            checkout_name: short.to_string(),
            is_current: false,
            is_remote: true,
            track: Some(full.to_string()),
        });
    }
    Ok(branches)
}

/// Git's messages in English, whatever the user's locale (#925): a
/// refusal to switch is told apart by its text
/// ([`local_changes_block_switch`]), which a translated git words otherwise.
/// `LANGUAGE` picks gettext's catalog ahead of `LC_ALL` and `LANG` and
/// leaves the character set alone; git has no `en` catalog, so its own
/// text is what prints.
pub(super) const ENGLISH_MESSAGES: (&str, &str) = ("LANGUAGE", "en");

/// Switch to an existing local branch (`git switch <name>`).
pub fn checkout_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation_with_env(root, &["switch", name], &[ENGLISH_MESSAGES])
}

/// Check out a remote-tracking branch as local branch `name` tracking it
/// (`git switch -c feat --track upstream/feat`): the remote the user
/// picked, even when another remote has a branch of the same name (#1445).
/// The name is spelled out because `--track` alone guesses it by cutting
/// at the first `/`, which makes `my/fork/solo` a `fork/solo` branch.
pub fn track_remote_branch(root: &Path, remote_ref: &str, name: &str) -> Result<String, String> {
    run_mutation_with_env(
        root,
        &["switch", "-c", name, "--track", remote_ref],
        &[ENGLISH_MESSAGES],
    )
}

/// Create a new branch off the current HEAD and switch to it
/// (`git switch -c <name>`). Errors (e.g. name already exists) carry
/// git's verbatim message.
pub fn create_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "-c", name])
}

// --- Branch management ---------------------------------------------------

/// Create a new branch off an explicit base ref and switch to it
/// (`git switch -c <name> <base>`). Powers "Create Branch from…".
pub fn create_branch_from(root: &Path, name: &str, base: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "-c", name, base])
}

/// Rename the current branch (`git branch -m <new>`).
pub fn rename_branch(root: &Path, new_name: &str) -> Result<String, String> {
    run_mutation(root, &["branch", "-m", new_name])
}

/// Delete a branch (`git branch -d <name>`). Uses the safe `-d`, which
/// refuses to drop unmerged work; the error is surfaced verbatim so the
/// user can decide whether to force.
pub fn delete_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["branch", "-d", name])
}

/// Merge `branch` into the current branch (`git merge <branch>`).
pub fn merge_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["merge", branch])
}

/// Rebase the current branch onto `branch` (`git rebase <branch>`).
pub fn rebase_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["rebase", branch])
}
