//! Committing: refusing unmerged paths and leftover conflict markers, committing and
//! amending, undoing the last commit, and the commit graph's actions (cherry-pick, revert,
//! checkout, tag, reset).

use super::*;

/// Commit the way the Commit button, Enter in the message box, Commit &
/// Push and Commit & Sync do, by VS Code's rule (#965): with anything
/// staged, only the index (`git commit -m`), so hunks staged by hand go in
/// and the rest stays in the working tree; with nothing staged, every
/// tracked change (`git commit -am`, VS Code's smart commit). The index is
/// read at commit time, not from the panel's last status poll. Refuses an
/// empty message and an unresolved merge ([`refuse_unmerged`]); on failure
/// returns git's message verbatim so the user sees exactly what blocked them.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    refuse_unmerged(root)?;
    let flags = if has_staged_changes(root)? {
        "-m"
    } else {
        "-am"
    };
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    // A hook or signing helper that wants a terminal fails fast with its
    // own message instead of fighting croft for the screen (#1153).
    let mut cmd = crate::proc::command("git");
    cmd.args(["-C", path_str, "commit", flags, message]);
    never_prompt(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let summary = if !stdout.is_empty() { stdout } else { stderr };
        Ok(summary)
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git commit failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

// --- Commit variants -----------------------------------------------------

/// Paths git still holds as unmerged (`UU`, `AA`, `DU`, …): the MERGE
/// CONFLICTS group, read straight from the index.
pub fn unmerged_paths(root: &Path) -> Vec<String> {
    let Ok(out) = crate::proc::command("git")
        .arg("-C")
        .arg(root)
        // `--no-relative`: with `diff.relative` set, a subdirectory root
        // would hide the conflicts outside it, and `git add -A` stages them.
        .args(["diff", "--no-relative", "--name-only", "--diff-filter=U", "-z"])
        .output()
    else {
        return Vec::new();
    };
    let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    paths.dedup();
    paths
}

/// Refuse to commit while a merge still has unresolved conflicts (#989).
/// `git commit -a` and `git add -A` stage an unmerged file as it is, so the
/// commit would record its `<<<<<<<` markers; a plain `git commit` refuses
/// here by itself, and this says the same thing up front for the paths that
/// stage on the user's behalf.
pub fn refuse_unmerged(root: &Path) -> Result<(), String> {
    let paths = unmerged_paths(root);
    if paths.is_empty() {
        return Ok(());
    }
    let named = first_paths(&paths);
    let (n, them) = match paths.len() {
        1 => (String::from("1 merge conflict"), "it"),
        n => (format!("{n} merge conflicts"), "them"),
    };
    Err(format!(
        "Resolve {n} ({named}) and stage {them} before committing"
    ))
}

/// Whether the index holds anything a plain `git commit` would record: a
/// change against HEAD, or on an unborn branch any staged file.
pub(super) fn has_staged_changes(root: &Path) -> Result<bool, String> {
    let output = crate::proc::command("git")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // `--no-relative`: with `diff.relative` set, a subdirectory root
        // would see only what is staged under it, and a commit takes it all.
        // `--ignore-submodules=none`: `diff.ignoreSubmodules` hides a staged
        // submodule update from diff, but `git commit` still records it.
        // `--no-ext-diff`: an external diff helper whose exit code is trusted
        // (`diff.trustExitCode`, git 2.46+) could call a staged change equal.
        .args([
            "diff",
            "--cached",
            "--quiet",
            "--no-relative",
            "--ignore-submodules=none",
            "--no-ext-diff",
        ])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        code => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("git diff --cached failed with code {code:?}")
            } else {
                stderr
            })
        }
    }
}

/// Up to three of `paths`, comma-separated, with `…` when there are more.
pub(super) fn first_paths(paths: &[String]) -> String {
    let mut named = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 3 {
        named.push_str(", …");
    }
    named
}

/// Refuse to stage a conflicted file that still holds its conflict markers
/// (#989). `git add` is how git marks a conflict resolved, and it takes the
/// file as it is, so Stage All, or staging the file itself, would record
/// the `<<<<<<<` lines and the next commit would ship them. A conflicted
/// file whose markers have been edited away stages as usual. `scope` holds
/// the `root`-relative paths about to be staged; `None` means everything.
/// The markers are found by [`leftover_conflict_markers`].
pub(super) fn refuse_conflict_markers(root: &Path, scope: Option<&[String]>) -> Result<(), String> {
    let unmerged = unmerged_paths(root);
    if unmerged.is_empty() {
        return Ok(());
    }
    // `unmerged_paths` names paths from the top of the repository, which
    // is not `root` when croft was opened on a subdirectory.
    let Some(top) = repo_toplevel(root) else {
        return Ok(());
    };
    let canon = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let scope: Option<Vec<PathBuf>> =
        scope.map(|paths| paths.iter().map(|p| canon(root.join(p))).collect());
    let marked: Vec<String> = unmerged
        .into_iter()
        .filter(|rel| {
            let abs = canon(top.join(rel));
            scope
                .as_ref()
                .is_none_or(|scope| scope.iter().any(|s| abs.starts_with(s)))
                && leftover_conflict_markers(&top, rel)
        })
        .collect();
    if marked.is_empty() {
        return Ok(());
    }
    let (has, them) = if marked.len() == 1 {
        ("has", "it")
    } else {
        ("have", "them")
    };
    Err(format!(
        "{} still {has} conflict markers: resolve {them} before staging",
        first_paths(&marked)
    ))
}

/// Whether git's own `diff --check` finds conflict markers in the unmerged
/// `rel` (a path from the top of the repository at `top`). Reading the
/// file ourselves missed three cases git handles: a file in Latin-1 or
/// Shift_JIS, markers of another length set by a `conflict-marker-size`
/// attribute, and marker lines both sides already had, such as a doc
/// showing what a conflict looks like, which would block staging forever.
pub(super) fn leftover_conflict_markers(top: &Path, rel: &str) -> bool {
    crate::proc::command("git")
        .arg("-C")
        .arg(top)
        .args(["--literal-pathspecs", "diff", "--check", "--no-color", "--"])
        .arg(rel)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("leftover conflict marker"))
}

/// Commit only what is already staged (`git commit -m`), leaving unstaged
/// changes in the working tree. With nothing staged git refuses it, where
/// [`commit`] would take every tracked change.
pub fn commit_staged(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    run_mutation(root, &["commit", "-m", message])
}

/// Amend the previous commit with a new message (`git commit --amend -m`),
/// folding any currently-staged changes into it. Rewrites history, so the
/// caller confirms before invoking.
pub fn commit_amend(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    run_mutation(root, &["commit", "--amend", "-m", message])
}

/// Amend the previous commit keeping its existing message
/// (`git commit --amend --no-edit`); folds staged changes into HEAD.
pub fn commit_amend_no_edit(root: &Path) -> Result<String, String> {
    run_mutation(root, &["commit", "--amend", "--no-edit"])
}

/// Undo Last Commit (#1350), as VS Code does it: `git reset --soft HEAD~1`,
/// so HEAD moves to the parent and the commit's changes stay staged, or
/// on the root commit `git update-ref -d HEAD`, which leaves the branch
/// unborn with every file staged. Returns the undone commit's message, for
/// the message box. Refused while a merge, rebase, cherry-pick or revert
/// is in progress: moving HEAD under one of those breaks it.
pub fn undo_last_commit(root: &Path) -> Result<String, String> {
    if let Some(op) = operation_in_progress(root) {
        return Err(format!(
            "a {op} is in progress; finish or abort it before undoing a commit"
        ));
    }
    let message = run_git(root, &["log", "-1", "--format=%B", "HEAD"])
        .map_err(|_| String::from("there is no commit to undo"))?;
    if run_git(root, &["rev-parse", "--verify", "-q", "HEAD~1"]).is_ok() {
        run_mutation(root, &["reset", "--soft", "HEAD~1"])?;
    } else {
        // A shallow checkout cuts history at a commit that still has a
        // parent: deleting the ref there would drop the branch tip.
        let raw = run_git(root, &["cat-file", "commit", "HEAD"])
            .map_err(|_| String::from("cannot read the last commit"))?;
        if raw
            .lines()
            .take_while(|l| !l.is_empty())
            .any(|l| l.starts_with("parent "))
        {
            return Err(String::from(
                "the parent commit is not in this shallow checkout; deepen it before undoing a commit",
            ));
        }
        // On a detached HEAD, `update-ref -d HEAD` deletes `.git/HEAD`
        // itself and the repository stops being one.
        if run_git(root, &["symbolic-ref", "-q", "HEAD"]).is_err() {
            return Err(String::from(
                "HEAD is detached; check out a branch before undoing its root commit",
            ));
        }
        run_mutation(root, &["update-ref", "-d", "HEAD"])?;
    }
    Ok(message)
}

/// Whether HEAD is already on its upstream (`HEAD` is an ancestor of
/// `@{u}`), so undoing it rewrites published history and needs a force
/// push later (#1350). No upstream is not pushed; an upstream that is
/// configured but cannot be checked (its tracking ref missing) counts as
/// pushed, so the warning errs on the side of showing.
pub fn head_is_on_upstream(root: &Path) -> bool {
    let code = crate::proc::command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", "HEAD", "@{u}"])
        .output()
        .ok()
        .and_then(|o| o.status.code());
    match code {
        Some(0) => true,
        Some(1) => false,
        _ => run_git(root, &["symbolic-ref", "-q", "HEAD"])
            .and_then(|branch| run_git(root, &["for-each-ref", "--format=%(upstream)", &branch]))
            .is_ok_and(|upstream| !upstream.is_empty()),
    }
}

// --- Commit actions (the COMMITS graph's right-click menu, #1627) ---------

/// Apply `hash` on top of HEAD (`git cherry-pick <hash>`). A conflict
/// leaves `CHERRY_PICK_HEAD`, so Git: Continue / Skip / Abort take over.
pub fn cherry_pick(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["cherry-pick", hash])
}

/// Commit the inverse of `hash` with git's own "Revert ..." message
/// (`git revert --no-edit <hash>`); a conflict leaves `REVERT_HEAD`.
pub fn revert_commit(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["revert", "--no-edit", hash])
}

/// Check out `hash` with no branch (`git switch --detach <hash>`).
pub fn checkout_detached(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "--detach", hash])
}

/// Create a lightweight tag at `target` (`git tag <name> <target>`).
pub fn create_tag_at(root: &Path, name: &str, target: &str) -> Result<String, String> {
    run_mutation(root, &["tag", name, target])
}

/// How far [`reset_to`] rewinds: HEAD only, HEAD and the index, or HEAD,
/// the index and the working tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

impl ResetMode {
    pub(super) fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        }
    }
}

/// Move the current branch to `hash` (`git reset --soft|--mixed|--hard`).
pub fn reset_to(root: &Path, hash: &str, mode: ResetMode) -> Result<String, String> {
    run_mutation(root, &["reset", "-q", mode.flag(), hash])
}

/// A snapshot of what a hard reset would throw away: HEAD and the branch
/// it is on (empty when detached), the index (each entry's blob, so
/// staging another hunk of an already-staged file changes it), the status
/// of every changed or untracked path, and each such file's size and
/// mtime, so a save to an already-modified file changes it too. A warned
/// Reset --hard (#1627) is confirmed only while this is unchanged.
pub fn worktree_fingerprint(root: &Path) -> String {
    let head = run_git(root, &["rev-parse", "HEAD"]).unwrap_or_default();
    let branch = run_git(root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let index = run_git(root, &["ls-files", "-s", "-z"]).unwrap_or_default();
    // Not `run_git`: its trim would eat the first entry's leading status
    // space (" M path"), and with it where that path starts.
    let status = crate::proc::command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let top = repo_toplevel(root).unwrap_or_else(|| root.to_path_buf());
    let mut out = format!("{head}\n{branch}\n{index}\n");
    for entry in status.split('\0').filter(|e| !e.is_empty()) {
        out.push_str(entry);
        let path = entry.get(3..).unwrap_or(entry);
        if let Ok(meta) = std::fs::metadata(top.join(path)) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.push_str(&format!(" {} {mtime}", meta.len()));
        }
        out.push('\n');
    }
    out
}
