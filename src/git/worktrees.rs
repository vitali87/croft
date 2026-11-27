//! Worktrees: a lane's branch in a sibling folder of its own, checking a pull request out
//! into a worktree, and the reasons a worktree is kept rather than removed.

use super::*;

/// A worktree lane: a branch and the sibling directory it lives in (#348).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeLane {
    /// Absolute path of the worktree, a SIBLING of the repo.
    pub path: PathBuf,
    /// The branch created for the lane.
    pub branch: String,
    /// The branch the lane was cut FROM, recorded at creation (#348).
    ///
    /// Not derivable later. `worktree add -b` takes no start-point, so the
    /// lane forks from whatever HEAD was; git records that only in the
    /// reflog, which is local, prunable and absent from a fresh clone — so a
    /// lane made on one machine has no base to recover on another.
    /// `merge-base` cannot help either: it needs the candidate, which is the
    /// thing being asked for. Creation is the only moment the answer is free.
    ///
    /// `None` when HEAD was detached at creation, which has no branch name
    /// to record and must not be guessed at.
    pub base: Option<String>,
}

impl WorktreeLane {
    /// Plan a lane for `name` beside `repo`, or `None` when the name has
    /// nothing usable in it.
    ///
    /// Refused rather than defaulted: a name of `"..."` would otherwise
    /// become the branch `agent/` and the directory `croft-`, and a branch
    /// named after nothing is worse than being told to pick a name.
    ///
    /// The slug is what makes both halves safe at once. git refuses a ref
    /// containing `..`, whitespace, or any of `~^:?*[\`, and a directory
    /// name carrying those is a different kind of trouble — so one pass
    /// reducing to `[a-z0-9-]` covers the ref grammar and the filesystem
    /// together, rather than sanitising twice with two chances to disagree.
    pub fn plan(repo: &Path, name: &str) -> Option<Self> {
        let slug = lane_slug(name)?;
        // A SIBLING, never a child. A worktree inside its own repo is a
        // working tree git then tries to track, and the Explorer would show
        // the lane nested in the root it was cut from.
        let parent = repo.parent()?;
        let base = repo.file_name()?.to_string_lossy();
        Some(Self {
            path: parent.join(format!("{base}-{slug}")),
            branch: format!("agent/{slug}"),
            // `plan` is pure - it runs no git. The base is filled in by
            // `add_worktree_lane`, which is already running git and is the
            // moment HEAD is still the branch being forked from.
            base: None,
        })
    }
}

/// Reduce `name` to `[a-z0-9-]`, or `None` when nothing survives.
pub(super) fn lane_slug(name: &str) -> Option<String> {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs so `a  b` is `a-b` rather than `a--b`, and trim so the
    // branch never starts or ends on the separator — git refuses a ref
    // component ending in `.`, and a trailing `-` merely looks broken.
    let mut out = String::new();
    for part in mapped.split('-').filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        return None;
    }
    // Capped, because the slug becomes a DIRECTORY name as well as a branch:
    // git accepts a ref of any length, but a filesystem component past ~255
    // bytes fails at `mkdir` with "File name too long" — measured — and the
    // user would see that instead of anything about lanes. 60 is far more
    // than a lane name needs. Trimmed again because the cut can land on the
    // separator.
    let capped: String = out.chars().take(60).collect();
    let capped = capped.trim_matches('-');
    (!capped.is_empty()).then(|| capped.to_string())
}

/// Why `lane` must not be removed, or `None` when it is safe to remove.
///
/// `git worktree remove` DISCARDS a dirty tree, so this is the one check in
/// the lane flow where being wrong destroys work rather than annoying
/// someone. It answers with a reason rather than a bool so the caller can
/// tell the user what to do about it.
///
/// A tree git cannot report on is treated as unsafe: an unreadable status is
/// not evidence of cleanliness, and refusing costs a user one manual
/// `git worktree remove` while guessing costs them their work.
pub fn lane_removal_block(lane: &Path) -> Option<String> {
    let Some(path_str) = lane.to_str() else {
        return Some(format!("{} is not a readable path", lane.display()));
    };
    let output = Git::new()
        .at(path_str)
        .args(["status", "--porcelain", "--ignored"])
        .into_command()
        .output();
    let Ok(output) = output else {
        return Some(String::from(
            "could not run git to check for uncommitted work",
        ));
    };
    if !output.status.success() {
        return Some(format!("{path_str} does not look like a git worktree"));
    }
    let dirty = String::from_utf8_lossy(&output.stdout);
    let mut changed = 0usize;
    let mut ignored = 0usize;
    for line in dirty.lines().filter(|l| !l.trim().is_empty()) {
        // `!!` is an IGNORED entry, which `--ignored` adds and plain
        // `--porcelain` omits entirely. Counted separately because the two
        // want different words, but both must refuse: `git worktree remove`
        // deletes ignored files without a murmur, and an agent lane is
        // exactly where an uncommitted `.env`, a local database, or an
        // expensive `node_modules` lives. Measured — a `.env` in a lane was
        // destroyed while plain `--porcelain` reported the tree clean.
        if line.starts_with("!!") {
            ignored += 1;
        } else {
            changed += 1;
        }
    }
    match (changed, ignored) {
        (0, 0) => None,
        (0, n) => Some(format!(
            "this lane has {n} ignored file{} (build output, local config) that removal would \
             delete — clear them by hand first",
            if n == 1 { "" } else { "s" }
        )),
        (1, _) => Some(String::from(
            "this lane has 1 file with uncommitted changes — commit or discard it first",
        )),
        (n, _) => Some(format!(
            "this lane has {n} files with uncommitted changes — commit or discard them first"
        )),
    }
}

/// Add `lane` as a worktree of `repo` on a new branch.
pub fn add_worktree_lane(repo: &Path, lane: &mut WorktreeLane) -> Result<(), String> {
    let Some(path) = lane.path.to_str().map(str::to_owned) else {
        return Err(String::from("lane path is not valid UTF-8"));
    };
    // READ before the command, while HEAD is still the branch the lane forks
    // from - afterwards it is gone, see `WorktreeLane::base`. But ASSIGN only
    // once creation succeeded: a failed `worktree add` leaves no lane, and a
    // base recorded on one that does not exist is a fact about nothing that a
    // retry would then carry.
    let base = current_branch(repo);
    run_git_mut(repo, &["worktree", "add", "-b", &lane.branch, &path])?;
    lane.base = base;
    Ok(())
}

/// Remove `lane`'s worktree.
///
/// No `--force`: the caller has already asked [`lane_removal_block`], and a
/// force here would defeat that check by making the refusal advisory. If git
/// refuses anyway, the reason reaches the user rather than being overridden.
pub fn remove_worktree_lane(lane: &Path) -> Result<(), String> {
    let Some(path) = lane.to_str() else {
        return Err(String::from("lane path is not valid UTF-8"));
    };
    // Run from the lane itself: git resolves the main worktree from there,
    // so the caller does not have to remember which repo it was cut from.
    run_git_mut(lane, &["worktree", "remove", path])
}

/// Where review mode checks pull request `number` out (#365): a sibling of
/// `repo`, like a worktree lane, never a child git would then see.
pub fn pr_worktree_path(repo: &Path, number: u64) -> Option<PathBuf> {
    let name = repo.file_name()?.to_str()?;
    Some(repo.parent()?.join(format!("{name}-pr-{number}")))
}

/// Which remote to fetch `owner/repo`'s pull requests from: the one whose
/// URL names it (`git@github.com:o/r.git`, `https://github.com/o/r`), else
/// the repository's own URL, which `git fetch` takes as well.
pub fn remote_for_slug(repo: &Path, slug: &str) -> String {
    let slug = slug.to_ascii_lowercase();
    let names = |url: &str| {
        let url = url.trim().to_ascii_lowercase();
        let url = url.strip_suffix(".git").unwrap_or(&url).to_string();
        url.ends_with(&format!("/{slug}")) || url.ends_with(&format!(":{slug}"))
    };
    run_git(repo, &["remote", "-v"])
        .ok()
        .and_then(|out| {
            out.lines().find_map(|l| {
                let mut parts = l.split_whitespace();
                let (name, url) = (parts.next()?, parts.next()?);
                names(url).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| format!("https://github.com/{slug}.git"))
}

/// Whether `path` is one of `repo`'s worktrees.
pub(super) fn is_worktree_of(repo: &Path, path: &Path) -> bool {
    let want = path.canonicalize().ok();
    run_git(repo, &["worktree", "list", "--porcelain"]).is_ok_and(|out| {
        out.lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|w| Path::new(w).canonicalize().ok() == want)
    })
}

/// A pull request head checked out by [`checkout_pr_worktree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrWorktree {
    /// The commit checked out.
    pub oid: String,
    /// Whether this call created the worktree. One that was already there
    /// is the user's as much as review mode's, and leaving never removes it.
    pub created: bool,
}

/// Fetch pull request `number` from `remote` and check its head out,
/// detached, in a worktree at `path` (#365).
///
/// An existing worktree of the same repository is moved to the new head
/// only when that loses nothing: it is clean, and its HEAD is the fetched
/// head already or `previous`, the head review mode checked out there last.
/// Any other HEAD holds commits that a detached checkout would leave to the
/// reflog. Anything else already at `path` is refused, never touched.
pub fn checkout_pr_worktree(
    repo: &Path,
    remote: &str,
    number: u64,
    path: &Path,
    previous: Option<&str>,
) -> Result<PrWorktree, String> {
    let Some(path_s) = path.to_str() else {
        return Err(String::from("worktree path is not valid UTF-8"));
    };
    let Some(repo_s) = repo.to_str() else {
        return Err(String::from("repository path is not valid UTF-8"));
    };
    // No prompt: this runs off the UI thread, where a credential prompt,
    // or ssh's own passphrase or host-key question on /dev/tty, would wait
    // forever on a terminal nobody sees, or draw over the UI.
    let fetch = Git::new()
        .at(repo_s)
        .args(["fetch", "--quiet", remote])
        .arg(format!("refs/pull/{number}/head"))
        .never_prompting()
        .into_command()
        .output()
        .map_err(|e| e.to_string())?;
    if !fetch.status.success() {
        let err = String::from_utf8_lossy(&fetch.stderr);
        return Err(err
            .trim()
            .lines()
            .next()
            .unwrap_or("git fetch failed")
            .to_string());
    }
    let oid = run_git(repo, &["rev-parse", "FETCH_HEAD"])
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    if path.exists() {
        if !is_worktree_of(repo, path) {
            return Err(format!(
                "{path_s} already exists and is not a worktree of this repository"
            ));
        }
        let head = worktree_head(path);
        if head.as_deref() != Some(oid.as_str()) {
            if let Some(why) = lane_removal_block(path) {
                return Err(format!("the existing checkout has work in it: {why}"));
            }
            if previous.is_none() || head.as_deref() != previous {
                return Err(format!(
                    "{path_s} is at a commit review mode did not check out — put that work on a branch, or remove the worktree"
                ));
            }
        }
        run_git_mut(path, &["checkout", "--quiet", "--detach", &oid])?;
        Ok(PrWorktree {
            oid,
            created: false,
        })
    } else {
        run_git_mut(repo, &["worktree", "add", "--detach", path_s, &oid])?;
        Ok(PrWorktree { oid, created: true })
    }
}

/// The commit a worktree's HEAD is on.
pub fn worktree_head(path: &Path) -> Option<String> {
    run_git(path, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Why leaving review must keep the pull request's checkout: uncommitted or
/// ignored files (see [`lane_removal_block`]), or commits made on top of the
/// checked-out head, which a detached worktree would leave unreachable.
pub fn pr_worktree_keep_reason(path: &Path, checked_out: &str) -> Option<String> {
    if let Some(why) = lane_removal_block(path) {
        return Some(why);
    }
    match worktree_head(path) {
        Some(head) if head == checked_out => None,
        Some(_) => Some(String::from(
            "it has commits beyond the pull request's head — put them on a branch first",
        )),
        None => Some(String::from("git cannot read its HEAD")),
    }
}
