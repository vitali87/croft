//! History: a file's commits across renames, the commit graph, a branch's commits, a
//! commit's files and diffs, its message and hover card, and how long ago it was.

use super::*;

/// One commit in a file's history, for the Explorer TIMELINE view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHistoryEntry {
    pub short_hash: String,
    pub summary: String,
    pub author: String,
    /// Seconds elapsed since the commit time, for [`humanize_age`].
    pub age_secs: i64,
}

/// The folder to run a per-file query from, and the pathspec to pass it:
/// the file's own folder and name, so git finds the repo that holds the
/// file even when `root` (the workspace) sits above it (#1227). A
/// cwd-relative pathspec is exact with `-C`. Falls back to `root` and
/// `rel_path` when that folder is gone (a deleted file's history).
pub(super) fn file_cwd(root: &Path, rel_path: &str) -> (PathBuf, String) {
    let abs = root.join(rel_path);
    match (abs.parent(), abs.file_name().and_then(|n| n.to_str())) {
        (Some(dir), Some(name)) if dir.is_dir() => (dir.to_path_buf(), name.to_string()),
        _ => (root.to_path_buf(), rel_path.to_string()),
    }
}

/// The top of the repository that holds `rel_path`, found from the file's
/// own folder as [`file_cwd`] finds it, and the file's path from that top,
/// which is how the rename walk in [`path_at_commit`] names it. Git run in a
/// workspace that sits above a nested repository finds none (#1556). Falls
/// back to `root` and `rel_path` outside any repository.
pub(super) fn repo_top_and_path(root: &Path, rel_path: &str) -> (PathBuf, String) {
    let (dir, name) = file_cwd(root, rel_path);
    let out = run_git(&dir, &["rev-parse", "--show-toplevel", "--show-prefix"]);
    let mut lines = out.as_deref().unwrap_or_default().lines();
    match lines.next() {
        Some(top) if !top.is_empty() => {
            let prefix = lines.next().unwrap_or_default();
            (PathBuf::from(top), format!("{prefix}{name}"))
        }
        _ => (root.to_path_buf(), rel_path.to_string()),
    }
}

/// Recent commits touching `rel_path`, newest first, via `git log --follow`
/// (so the history survives renames, like VS Code's Timeline). Returns an
/// empty vec when the path is untracked, the repo has no history, or git is
/// unavailable — the panel renders that as an empty state, never an error.
pub fn file_history(root: &Path, rel_path: &str, limit: usize) -> Vec<FileHistoryEntry> {
    let (dir, name) = file_cwd(root, rel_path);
    let Some(path_str) = dir.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--follow",
            &format!("-n{limit}"),
            "--format=%h\x1f%s\x1f%an\x1f%ct",
            "--",
            &name,
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_file_history(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// Parse the `%h\x1f%s\x1f%an\x1f%ct` lines emitted by [`file_history`] into
/// entries, computing each commit's age relative to `now` (unix seconds).
pub fn parse_file_history(out: &str, now: i64) -> Vec<FileHistoryEntry> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let short_hash = parts.next()?.to_string();
            let summary = parts.next()?.to_string();
            let author = parts.next()?.to_string();
            let ct: i64 = parts.next()?.trim().parse().ok()?;
            if short_hash.is_empty() {
                return None;
            }
            Some(FileHistoryEntry {
                short_hash,
                summary,
                author,
                age_secs: now - ct,
            })
        })
        .collect()
}

/// Raw `git show <hash> -- <rel_path>` text: the file's diff in that commit,
/// for opening a TIMELINE entry in the side-by-side diff viewer. Run in the
/// repository that holds the file, wherever the workspace `root` sits.
pub fn show_commit_file_diff(root: &Path, hash: &str, rel_path: &str) -> Result<String, String> {
    let (top, path) = repo_top_and_path(root, rel_path);
    let (root, rel_path) = (top.as_path(), path.as_str());
    if path_filter(root, rel_path).is_some() {
        return filtered_commit_file_diff(root, hash, rel_path);
    }
    let has_diff = |raw: &str| raw.lines().any(|l| l.starts_with("diff --git"));
    let raw = diff_text(root, &["show", hash, "--", rel_path])?;
    if has_diff(&raw) {
        return Ok(raw);
    }
    // The TIMELINE lists history with `--follow`, so a commit from before a
    // rename touched the file under its OLD name, and showing it under the
    // current one was an empty diff. The follow walk says what it was called.
    match path_at_commit(root, hash, rel_path) {
        Some(old) if old != rel_path => diff_text(root, &["show", hash, "--", &old]),
        _ => Ok(raw),
    }
}

/// The commits that touched `rel_path`, newest first, each with the name
/// the file had there: `git log --follow`, so the walk carries on past a
/// rename under the old name (#1304). Paths are relative to `root`, as
/// [`read_file_at_rev`] takes them. Empty when git fails.
pub fn follow_names(root: &Path, rel_path: &str, limit: usize) -> Vec<(String, String)> {
    let Ok(out) = run_git(
        root,
        &[
            "log",
            "--follow",
            // The scrubber walks first parents: a rename merged from a side
            // branch then lands on the merge, which a full walk skips for
            // the side branch's own commits (#1304).
            "--first-parent",
            &format!("-n{limit}"),
            "--name-only",
            "--relative",
            "--format=%x1e%H",
            "--",
            rel_path,
        ],
    ) else {
        return Vec::new();
    };
    out.split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let hash = lines.next()?.trim();
            let path = lines.map(str::trim).find(|l| !l.is_empty())?;
            (!hash.is_empty()).then(|| (hash.to_string(), unquote_porcelain_path(path)))
        })
        .collect()
}

/// [`show_commit_file_diff`] for a filtered file (#1338). `git show` diffs
/// the stored blobs, which for Git LFS are pointers whose only change is the
/// `oid` line, so the two sides are read through the filter instead and
/// diffed here, under the commit's own `git show` header, in the shape the
/// viewer parses for an unfiltered file.
pub(super) fn filtered_commit_file_diff(
    root: &Path,
    hash: &str,
    rel_path: &str,
) -> Result<String, String> {
    let name = match read_file_at_rev(root, hash, rel_path) {
        Ok(_) => rel_path.to_string(),
        Err(_) => path_at_commit(root, hash, rel_path).unwrap_or_else(|| rel_path.to_string()),
    };
    let new = read_file_at_rev(root, hash, &name)?;
    // Absent before this commit (the file was added here, or this is the
    // root commit): every line is new.
    let old = read_file_at_rev(root, &format!("{hash}^"), &name).unwrap_or_default();
    let body = similar::TextDiff::from_lines(&old, &new)
        .unified_diff()
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string();
    let header = diff_text(root, &["show", "-s", hash])?;
    Ok(format!("{header}\ndiff --git a/{name} b/{name}\n{body}"))
}

/// The name `rel_path` had in commit `hash`, from the same `--follow` walk
/// [`file_history`] lists. None when the commit is not in that history.
pub(super) fn path_at_commit(root: &Path, hash: &str, rel_path: &str) -> Option<String> {
    let out = run_git(
        root,
        &[
            "log",
            "--follow",
            // As deep as the TIMELINE lists (`file_history`'s 50), with room
            // to spare: a row past it cannot be clicked, and the full walk
            // of a long history was the slow part.
            "-n200",
            "--name-only",
            "--format=%x1e%H",
            "--",
            rel_path,
        ],
    )
    .ok()?;
    out.split('\x1e').find_map(|record| {
        let mut lines = record.lines();
        let full = lines.next()?.trim();
        if hash.is_empty() || !full.starts_with(hash) {
            return None;
        }
        lines
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(unquote_porcelain_path)
    })
}

/// The whole commit as text — header, message, diffstat, and full patch —
/// for the Source Control graph's click-to-open view (the tig / lazygit
/// idiom for inspecting a commit in a terminal).
pub fn show_commit(root: &Path, hash: &str) -> Result<String, String> {
    diff_text(root, &["show", "--stat", "--patch", hash])
}

/// One commit of the repo-wide Source Control graph, straight off
/// `git log --topo-order` with its parent hashes (the lane layout's input)
/// and its decorations (branch / tag / HEAD ref names).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphCommit {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    /// Decoration names as git prints them (`HEAD -> main`, `origin/main`,
    /// `tag: v1.0`), split per ref; empty for an undecorated commit.
    pub refs: Vec<String>,
    pub summary: String,
    pub author: String,
    /// Seconds elapsed since the commit time, for [`humanize_age`].
    pub age_secs: i64,
}

/// The newest `limit` commits across local branches, tags, and HEAD in
/// topological order (parents never precede children), the input to the
/// Source Control graph. Empty on any failure — no repo, no commits, no git —
/// so the panel renders an empty state, never an error.
pub fn commit_graph(root: &Path, limit: usize) -> Vec<GraphCommit> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--branches",
            "--tags",
            "HEAD",
            "--topo-order",
            &format!("-n{limit}"),
            "--format=%H\x1f%h\x1f%P\x1f%D\x1f%s\x1f%an\x1f%ct",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_commit_graph(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// The current branch's first-parent history, newest first (#371).
///
/// NOT [`commit_graph`], which logs `--branches --tags HEAD` to draw the
/// repo-wide graph: with any other branch present that list interleaves
/// commits the current branch does not contain, and topological order can
/// put one of them at index 0. A scrubber built on it steps back from the
/// working tree onto a commit that is not HEAD and may not even be an
/// ancestor of it.
///
/// `--first-parent` because a scrubber walks the branch as a line. A merge's
/// second parent is a different line of development, and stepping into one
/// would take the user somewhere they cannot step back out of in a straight
/// line.
///
/// Empty on any failure — no repo, no commits, no git — so the caller shows
/// an empty state rather than an error, matching `commit_graph`.
pub fn branch_history(root: &Path, limit: usize) -> Vec<GraphCommit> {
    branch_history_range(root, "HEAD", limit)
}

/// [`branch_history`] over an explicit revision spec rather than `HEAD`.
///
/// Takes the spec as a string so a caller can pass a range: `base..lane`
/// lists the commits a lane added and none of the base's, which is what
/// "the branch's diff against the base" means for a worktree lane (#348).
/// `HEAD` reproduces the unranged behaviour exactly, which is why
/// `branch_history` is now a one-line call rather than a second copy of
/// this body.
///
/// Empty on any failure, like its caller: an unknown revision is a git
/// error, and the graph shows an empty state rather than an error row.
pub fn branch_history_range(root: &Path, revspec: &str, limit: usize) -> Vec<GraphCommit> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--first-parent",
            revspec,
            &format!("-n{limit}"),
            "--format=%H\x1f%h\x1f%P\x1f%D\x1f%s\x1f%an\x1f%ct",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    parse_commit_graph(&String::from_utf8_lossy(&output.stdout), now)
}

/// Commit `rev`'s tree under a workspace root, as [`tree_paths`] lists it
/// for the Explorer's dimming while the scrubber sits on a commit (#371).
#[derive(Debug, Default)]
pub struct CommitTree {
    /// Every path in the tree, absolute, directories included.
    pub paths: HashSet<PathBuf>,
    /// Submodule (gitlink) paths. `ls-tree` never lists their contents, so
    /// anything beneath one counts as present rather than dimming a whole
    /// submodule checkout.
    pub submodules: Vec<PathBuf>,
}

impl CommitTree {
    /// Whether `path` existed in the commit: listed, or inside a submodule.
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path) || self.submodules.iter().any(|s| path.starts_with(s))
    }
}

/// Every path in commit `rev`'s tree under `root`, absolute (`root`
/// joined), directories included — what the Explorer dims against while
/// the scrubber sits on a commit (#371).
///
/// `ls-tree` run with `-C root` and no `--full-tree` lists only the
/// entries under `root` and prints them relative to it, the same base
/// [`read_file_at_rev`]'s `./` gives, so a workspace opened in a
/// subdirectory of a repo joins onto its own root rather than the
/// toplevel. `-t` emits each directory on the way down, so a folder that
/// existed keeps its normal colour without deriving ancestors here.
///
/// `None` on any failure (no repo, unknown revision), so a caller dims
/// nothing rather than everything.
pub fn tree_paths(root: &Path, rev: &str) -> Option<CommitTree> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["ls-tree", "-r", "-t", "-z", rev])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut tree = CommitTree::default();
    // Each entry is `<mode> <type> <object>\t<path>`.
    for entry in output.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let path = join_raw(root, &entry[tab + 1..]);
        if entry[..tab].split(|b| *b == b' ').nth(1) == Some(b"commit") {
            tree.submodules.push(path.clone());
        }
        tree.paths.insert(path);
    }
    Some(tree)
}

/// Parse the `%H\x1f%h\x1f%P\x1f%D\x1f%s\x1f%an\x1f%ct` lines emitted by
/// [`commit_graph`], computing each commit's age relative to `now`.
pub fn parse_commit_graph(out: &str, now: i64) -> Vec<GraphCommit> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let hash = parts.next()?.to_string();
            let short_hash = parts.next()?.to_string();
            let parents: Vec<String> = parts
                .next()?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let refs: Vec<String> = parts
                .next()?
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let summary = parts.next()?.to_string();
            let author = parts.next()?.to_string();
            let ct: i64 = parts.next()?.trim().parse().ok()?;
            if hash.is_empty() {
                return None;
            }
            Some(GraphCommit {
                hash,
                short_hash,
                parents,
                refs,
                summary,
                author,
                age_secs: (now - ct).max(0),
            })
        })
        .collect()
}

/// The full message of `hash` (`git log -1 --format=%B`), trimmed.
pub fn commit_message(root: &Path, hash: &str) -> Result<String, String> {
    run_git(root, &["log", "-1", "--format=%B", hash])
        .map(|m| m.trim().to_string())
        .map_err(|e| e.to_string())
}

/// The inline blame's hover card for `hash` (#1556): author and email, the
/// full date, the short hash and the whole message, which `git blame`
/// does not give (its `summary` is the first line). `--no-show-signature`
/// because a `log.showSignature = true` config would otherwise print gpg's
/// verdict above the card.
pub fn commit_card(dir: &Path, hash: &str) -> Result<String, String> {
    let format = "--format=%an <%ae>%n%ad (%ar)%ncommit %h%n%n%B";
    run_git(dir, &["log", "-1", "--no-show-signature", format, hash]).map_err(|e| e.to_string())
}

/// Render an elapsed-seconds count as a coarse relative time (e.g.
/// "2 hours ago"), used by the Explorer TIMELINE view. Negative inputs
/// clamp to zero.
pub fn humanize_age(secs_ago: i64) -> String {
    let s = secs_ago.max(0);
    if s < 60 {
        return format!("{s} seconds ago");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m} minute{} ago", if m == 1 { "" } else { "s" });
    }
    let h = m / 60;
    if h < 24 {
        return format!("{h} hour{} ago", if h == 1 { "" } else { "s" });
    }
    let d = h / 24;
    if d < 30 {
        return format!("{d} day{} ago", if d == 1 { "" } else { "s" });
    }
    let mo = d / 30;
    if mo < 12 {
        return format!("{mo} month{} ago", if mo == 1 { "" } else { "s" });
    }
    let y = mo / 12;
    format!("{y} year{} ago", if y == 1 { "" } else { "s" })
}

pub(super) fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
