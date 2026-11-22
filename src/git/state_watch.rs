//! Noticing git used outside croft (#2577). A commit, checkout, staging,
//! fetch or push run in another terminal changes only files in git's own
//! folder, which the file watcher skips (its write storms are what #898
//! fixed). So the few files those commands write are stamped on the
//! watcher's poll instead, and a change to one re-reads the repository's
//! status, as VS Code does when its `.git` watcher fires.

use super::*;
use std::time::SystemTime;

/// A file's modification time and length, which git changes whenever it
/// rewrites or appends to the file; `None` while the file does not exist.
type Stamp = Option<(SystemTime, u64)>;

/// Where git keeps a repository's own state. The git dir holds what is per
/// worktree (HEAD and its reflog, the index, `FETCH_HEAD`); the common dir
/// holds what every worktree shares (branch refs, `packed-refs`). They are
/// the same folder except in a linked worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Work out [`GitDirs`] for the repository whose toplevel is `repo_root`
/// the way git does, from the filesystem alone, so the poll never starts
/// a process: `.git` is the git dir, or a file naming it (`gitdir: …`, in a
/// linked worktree or a submodule), and a `commondir` file inside the git
/// dir names the common dir. Either may be relative to where it is read.
pub fn git_dirs(repo_root: &Path) -> Option<GitDirs> {
    let dot = repo_root.join(".git");
    let git_dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let named = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        repo_root.join(named)
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim()),
        Err(_) => git_dir.clone(),
    };
    Some(GitDirs {
        git_dir,
        common_dir,
    })
}

/// The files git writes when a command changes what Source Control shows.
fn watched_files(dirs: &GitDirs, branch: Option<&str>, upstream: Option<&str>) -> Vec<PathBuf> {
    let mut files = vec![
        // A checkout rewrites HEAD; a commit, reset, merge, rebase or pull
        // appends to its reflog.
        dirs.git_dir.join("HEAD"),
        dirs.git_dir.join("logs").join("HEAD"),
        // Staging and unstaging.
        dirs.git_dir.join("index"),
        // A fetch, and so a pull.
        dirs.git_dir.join("FETCH_HEAD"),
        // Refs packed away, and refs moved while packed.
        dirs.common_dir.join("packed-refs"),
    ];
    // A commit moves the branch even where reflogs are turned off.
    if let Some(branch) = branch {
        files.push(dirs.common_dir.join("refs").join("heads").join(branch));
    }
    // A push moves the upstream, which the ahead count is measured from.
    if let Some(upstream) = upstream {
        files.push(dirs.common_dir.join(upstream));
    }
    files
}

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Watches one repository's git state for changes made outside croft.
#[derive(Default)]
pub struct GitStateWatch {
    /// The repository the stamps were taken in, and its git folders.
    repo: Option<(PathBuf, Option<GitDirs>)>,
    stamps: Vec<(PathBuf, Stamp)>,
}

impl GitStateWatch {
    /// Whether git's state for the repository `status` describes moved
    /// since the last call. The first call for a repository only takes the
    /// stamps. When `status` names another branch or upstream, their files
    /// are stamped afresh while the files every branch shares are still
    /// compared, so a change made while status was being re-read is kept.
    /// croft's own git reads change none of these files: they run without
    /// git's optional locks, so the index is never rewritten by a status.
    pub fn moved(&mut self, status: &GitStatus) -> bool {
        let Some(root) = status.repo_root.as_ref() else {
            *self = Self::default();
            return false;
        };
        let fresh = self.repo.as_ref().is_none_or(|(r, _)| r != root);
        if fresh {
            self.repo = Some((root.clone(), git_dirs(root)));
            self.stamps.clear();
        }
        let Some((_, Some(dirs))) = &self.repo else {
            return false;
        };
        let now: Vec<(PathBuf, Stamp)> =
            watched_files(dirs, status.branch.as_deref(), status.upstream.as_deref())
                .into_iter()
                .map(|path| {
                    let stamp = stamp(&path);
                    (path, stamp)
                })
                .collect();
        let moved = now.iter().any(|(path, stamp)| {
            self.stamps
                .iter()
                .any(|(seen, before)| seen == path && before != stamp)
        });
        self.stamps = now;
        moved
    }
}
