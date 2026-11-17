//! Diffs: what is staged, against a branch or where it forked, against the previous
//! commit, and what changed in a submodule.

use super::*;

/// Raw `git diff --staged` text — everything currently in the index that
/// would land in the next commit. Empty string when nothing is staged
/// (matches git's own zero-output behaviour). Errors carry stderr verbatim
/// so the panel can surface "fatal: not a git repository" etc.
pub fn diff_staged(root: &Path) -> Result<String, String> {
    diff_text(root, &["diff", "--staged"])
}

/// The commit HEAD and `branch` last had in common (`git merge-base`), or
/// None when they share no history or `branch` does not resolve.
pub fn merge_base(root: &Path, branch: &str) -> Option<String> {
    diff_text(root, &["merge-base", "HEAD", branch])
        .ok()
        .map(|out| out.trim().to_string())
        .filter(|sha| !sha.is_empty())
}

/// The working tree against the point the current branch left `branch`
/// (its merge base), as a pull request diff shows it: every committed and
/// uncommitted change on this branch, and nothing `branch` gained since
/// (#1444). Diffing against `branch`'s tip listed that later work as this
/// branch deleting or reverting it. With no shared history it falls back
/// to the tip. Used by the Source Control dropdown's "View Changes vs
/// <default>" and that tab's refresh.
pub fn diff_against_branch(root: &Path, branch: &str) -> Result<String, String> {
    match merge_base(root, branch) {
        Some(base) => diff_text(root, &["diff", &base]),
        None => diff_text(root, &["diff", branch]),
    }
}

/// Raw `git diff HEAD~1` text — working-tree state versus the commit
/// before HEAD. Used by the Source Control dropdown's "View Changes vs
/// previous" so users can preview everything that changed since the last
/// commit. Errors (e.g. "fatal: ambiguous argument 'HEAD~1'" on a repo
/// with a single commit) carry stderr verbatim so the panel can surface
/// the reason instead of a silent no-op.
pub fn diff_previous_commit(root: &Path) -> Result<String, String> {
    diff_text(root, &["diff", "HEAD~1"])
}

pub(super) fn diff_text(root: &Path, args: &[&str]) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str]);
    match args.split_first() {
        // The patch croft parses, never the user's display settings
        // (#1447): `color.ui = always` wrapped every line in escape codes,
        // and a `diff.external` tool (difftastic) printed its own view in
        // place of the patch.
        Some((sub, rest)) if matches!(*sub, "diff" | "show") => {
            cmd.arg(sub)
                .args(["--no-color", "--no-ext-diff"])
                .args(rest);
        }
        _ => {
            cmd.args(args);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git {args:?} failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// Whether `rel_path` is a submodule: a gitlink (mode `160000`) in the
/// index or in HEAD (#1235). Porcelain status lists one as a plain path, so
/// a Changes row cannot tell on its own, and a gitlink has no blob for
/// `git show HEAD:<path>` and nothing for `git checkout -- <path>` to do.
pub fn is_submodule(root: &Path, rel_path: &str) -> bool {
    let gitlink = |args: &[&str]| {
        run_git(root, args).is_ok_and(|out| out.lines().any(|l| l.starts_with("160000 ")))
    };
    gitlink(&["--literal-pathspecs", "ls-files", "--stage", "--", rel_path])
        || gitlink(&["--literal-pathspecs", "ls-tree", "HEAD", "--", rel_path])
}

/// What changed in submodule `rel_path` since HEAD, as `git diff` reports a
/// submodule: the commits it moved across (`--submodule=log`), then its
/// own changes, staged or not (`--submodule=diff`), with paths inside it.
pub fn submodule_diff(root: &Path, rel_path: &str) -> Result<String, String> {
    let diff = |mode: &str| {
        run_git(
            root,
            &[
                "-c",
                "color.ui=never",
                "--literal-pathspecs",
                "diff",
                mode,
                "HEAD",
                "--",
                rel_path,
            ],
        )
        .map_err(|e| e.to_string())
    };
    let log = diff("--submodule=log")?;
    let content: String = diff("--submodule=diff")?
        .lines()
        .filter(|l| !l.starts_with("Submodule "))
        .map(|l| format!("{l}\n"))
        .collect();
    Ok(if content.is_empty() {
        log
    } else {
        format!("{log}\n{content}")
    })
}
