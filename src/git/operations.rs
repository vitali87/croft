//! A merge, rebase, cherry-pick, revert or bisect in progress: which one it is, the branch a
//! rebase is rewriting, and continuing, skipping or aborting it.

use super::*;

/// An operation git stopped half-way through (#1356): what the status bar
/// names next to the branch, and what Abort / Continue / Skip act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoOp {
    Merge,
    /// `step` of `total` commits, as git counts them.
    Rebase {
        step: usize,
        total: usize,
    },
    CherryPick,
    Revert,
    Bisect,
}

impl RepoOp {
    /// The status-bar tag: `MERGING`, `REBASING 2/5`, `CHERRY-PICKING`,
    /// `REVERTING`, `BISECTING`.
    pub fn label(self) -> String {
        match self {
            RepoOp::Merge => String::from("MERGING"),
            RepoOp::Rebase { step, total } => format!("REBASING {step}/{total}"),
            RepoOp::CherryPick => String::from("CHERRY-PICKING"),
            RepoOp::Revert => String::from("REVERTING"),
            RepoOp::Bisect => String::from("BISECTING"),
        }
    }

    /// The git command it belongs to, also its name in messages.
    pub fn command(self) -> &'static str {
        match self {
            RepoOp::Merge => "merge",
            RepoOp::Rebase { .. } => "rebase",
            RepoOp::CherryPick => "cherry-pick",
            RepoOp::Revert => "revert",
            RepoOp::Bisect => "bisect",
        }
    }
}

/// The operation `root`'s repository is in the middle of (#1356), read
/// from the state files git leaves in its directory: `rebase-merge/` or
/// `rebase-apply/` (with the step counters), `MERGE_HEAD`,
/// `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG`. Found through
/// `--git-path`, so a worktree reads its own. A `git am` (an
/// `rebase-apply/applying` marker) is not one of these and reads as none.
pub fn repo_operation(root: &Path) -> Option<RepoOp> {
    const NAMES: [&str; 6] = [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
    ];
    let mut args = vec!["rev-parse"];
    for name in NAMES {
        args.extend(["--git-path", name]);
    }
    let out = run_git(root, &args).ok()?;
    let paths: Vec<PathBuf> = out.lines().map(|l| root.join(l.trim())).collect();
    if paths.len() != NAMES.len() {
        return None;
    }
    let counter = |dir: &Path, name: &str| -> usize {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };
    if paths[0].is_dir() {
        return Some(RepoOp::Rebase {
            step: counter(&paths[0], "msgnum"),
            total: counter(&paths[0], "end"),
        });
    }
    if paths[1].is_dir() && !paths[1].join("applying").exists() {
        return Some(RepoOp::Rebase {
            step: counter(&paths[1], "next"),
            total: counter(&paths[1], "last"),
        });
    }
    let op = [
        RepoOp::Merge,
        RepoOp::CherryPick,
        RepoOp::Revert,
        RepoOp::Bisect,
    ]
    .into_iter()
    .zip(&paths[2..])
    .find(|(_, p)| p.is_file())?;
    Some(op.0)
}

/// Abort `op` (#1356): `merge --abort`, `rebase --abort`,
/// `cherry-pick --abort`, `revert --abort`, or `bisect reset` for a bisect.
pub fn abort_operation(root: &Path, op: RepoOp) -> Result<String, String> {
    match op {
        RepoOp::Bisect => run_mutation(root, &["bisect", "reset"]),
        _ => run_mutation(root, &[op.command(), "--abort"]),
    }
}

/// Continue `op` once its conflicts are resolved and staged (#1356),
/// keeping each commit's own message: git's editor is `true`, so it never
/// waits for one. A merge is concluded by a commit, and a bisect has no
/// continue; both are refused.
pub fn continue_operation(root: &Path, op: RepoOp) -> Result<String, String> {
    match op {
        RepoOp::Merge => Err(String::from("Commit to conclude the merge, or abort it")),
        RepoOp::Bisect => Err(String::from(
            "Mark the commit good or bad to go on with the bisect",
        )),
        _ => run_mutation_with_env(
            root,
            &[op.command(), "--continue"],
            &[("GIT_EDITOR", "true")],
        ),
    }
}

/// Skip the commit `op` stopped on (#1356): `rebase --skip`,
/// `cherry-pick --skip`, `revert --skip`. Refused for a merge or bisect.
pub fn skip_operation(root: &Path, op: RepoOp) -> Result<String, String> {
    match op {
        RepoOp::Merge | RepoOp::Bisect => Err(format!("A {} has no skip", op.command())),
        _ => run_mutation_with_env(root, &[op.command(), "--skip"], &[("GIT_EDITOR", "true")]),
    }
}

/// Mark the commit a bisect checked out good or bad (#1356).
pub fn bisect_mark(root: &Path, good: bool) -> Result<String, String> {
    run_mutation(root, &["bisect", if good { "good" } else { "bad" }])
}

/// The branch the rebase in progress is rewriting (#1647), from the
/// `head-name` git writes in `rebase-merge/` or `rebase-apply/`
/// (`refs/heads/feature`). A rebase of a detached HEAD writes `detached
/// HEAD` there, which names no branch.
pub(super) fn rebasing_branch(root: &Path) -> Option<String> {
    let out = run_git(
        root,
        &[
            "rev-parse",
            "--git-path",
            "rebase-merge/head-name",
            "--git-path",
            "rebase-apply/head-name",
        ],
    )
    .ok()?;
    out.lines()
        .find_map(|l| std::fs::read_to_string(root.join(l.trim())).ok())
        .and_then(|name| {
            name.trim()
                .strip_prefix("refs/heads/")
                .filter(|b| !b.is_empty())
                .map(String::from)
        })
}

/// The git operation in progress in `root`'s repository, by the state
/// file git keeps for it, or `None`.
pub(super) fn operation_in_progress(root: &Path) -> Option<&'static str> {
    const STATES: [(&str, &str); 5] = [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
    ];
    let mut args = vec!["rev-parse"];
    for (file, _) in STATES {
        args.extend(["--git-path", file]);
    }
    let paths = run_git(root, &args).ok()?;
    paths
        .lines()
        .zip(STATES)
        .find(|(path, _)| root.join(path.trim()).exists())
        .map(|(_, (_, op))| op)
}
