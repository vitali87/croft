use std::path::{Path, PathBuf};
use std::process::Command;

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
}

pub fn query(root: &Path) -> GitStatus {
    if !is_git_repo(root) {
        return GitStatus::default();
    }
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
    let porcelain = run_git(root, &["status", "--porcelain"]).unwrap_or_default();
    let dirty = parse_porcelain_dirty(&porcelain);
    let (ahead, behind) =
        match run_git(root, &["rev-list", "--left-right", "--count", "HEAD...@{u}"]) {
            Ok(s) => parse_ahead_behind(&s),
            Err(_) => (0, 0),
        };
    GitStatus {
        in_repo: true,
        branch,
        detached_hash,
        dirty,
        ahead,
        behind,
    }
}

fn is_git_repo(path: &Path) -> bool {
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return false,
    };
    Command::new("git")
        .args(["-C", path_str, "rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, args: &[&str]) -> std::io::Result<String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| std::io::Error::other("non-utf8 workspace path"))?;
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str]).args(args);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {:?} failed with code {:?}",
            args,
            output.status.code()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// `git status --porcelain` prints one line per changed entry.
pub fn parse_porcelain_dirty(out: &str) -> bool {
    out.lines().any(|l| !l.trim().is_empty())
}

/// Working-tree change reported by `git status --porcelain`. The Source
/// Control panel groups entries by `kind.section()` and renders a one-letter
/// badge per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeEntry {
    /// Workspace-relative path. For renames this is the destination path.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    StagedAdded,
    StagedModified,
    StagedDeleted,
    StagedRenamed,
    Modified,
    Deleted,
    Untracked,
    Conflicted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSection {
    Staged,
    Changes,
    Untracked,
    Conflicts,
}

impl ChangeKind {
    pub fn section(self) -> ChangeSection {
        match self {
            Self::StagedAdded
            | Self::StagedModified
            | Self::StagedDeleted
            | Self::StagedRenamed => ChangeSection::Staged,
            Self::Modified | Self::Deleted => ChangeSection::Changes,
            Self::Untracked => ChangeSection::Untracked,
            Self::Conflicted => ChangeSection::Conflicts,
        }
    }

    pub fn badge(self) -> char {
        match self {
            Self::StagedAdded => 'A',
            Self::StagedModified | Self::Modified => 'M',
            Self::StagedDeleted | Self::Deleted => 'D',
            Self::StagedRenamed => 'R',
            Self::Untracked => 'U',
            Self::Conflicted => '!',
        }
    }
}

/// Parse `git status --porcelain` (v1) into the change entries the Source
/// Control panel renders. Each line is `XY <path>` where `X` is the index
/// status and `Y` is the worktree status; renames carry both old and new
/// paths separated by ` -> `.
pub fn parse_porcelain_changes(out: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.len() < 3 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path_part = &line[3..];
        let path = if let Some((_, dst)) = path_part.split_once(" -> ") {
            dst.to_string()
        } else {
            path_part.to_string()
        };
        // Conflicts: AA, DD, AU, UA, DU, UD, UU.
        let conflict = matches!((x, y),
            ('A', 'A') | ('D', 'D') | ('A', 'U') | ('U', 'A')
            | ('D', 'U') | ('U', 'D') | ('U', 'U'));
        if conflict {
            entries.push(ChangeEntry { path, kind: ChangeKind::Conflicted });
            continue;
        }
        if x == '?' && y == '?' {
            entries.push(ChangeEntry { path, kind: ChangeKind::Untracked });
            continue;
        }
        if x != ' ' && x != '?' {
            let kind = match x {
                'A' => ChangeKind::StagedAdded,
                'M' => ChangeKind::StagedModified,
                'D' => ChangeKind::StagedDeleted,
                'R' | 'C' => ChangeKind::StagedRenamed,
                _ => ChangeKind::StagedModified,
            };
            entries.push(ChangeEntry { path: path.clone(), kind });
        }
        if y != ' ' && y != '?' {
            let kind = match y {
                'M' => ChangeKind::Modified,
                'D' => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            entries.push(ChangeEntry { path, kind });
        }
    }
    entries
}

/// Run `git status --porcelain` against `root` and return parsed entries.
/// Returns an empty vec on any error so the panel renders cleanly even
/// when the workspace isn't a git repo or git is missing.
///
/// Does not call `run_git`: that helper trims stdout, which destroys the
/// fixed-width porcelain v1 format (a line like ` M README.md` becomes
/// `M README.md` after `.trim()`, shifting the status code by one column).
pub fn query_changes(root: &Path) -> Vec<ChangeEntry> {
    if !is_git_repo(root) {
        return Vec::new();
    }
    let path_str = match root.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let output = match Command::new("git")
        .args(["-C", path_str, "status", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let raw = String::from_utf8_lossy(&output.stdout);
    parse_porcelain_changes(&raw)
}

/// Result of an attempted commit. `Ok(summary)` carries git's stdout/stderr
/// summary (e.g. "[main 4a5b6c7] message"); `Err` carries git's error
/// Read the HEAD-committed contents of `rel_path` (workspace-relative)
/// via `git show HEAD:<rel_path>`. Used by the Source Control panel to
/// surface a side-by-side diff between the working tree and HEAD when
/// the user clicks a Modified entry. Returns the raw bytes as a String;
/// non-UTF8 content (binary file) is reported as an error so the caller
/// can fall back to opening the file directly.
pub fn read_file_at_head(root: &Path, rel_path: &str) -> Result<String, String> {
    let path_str = root.to_str().ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let spec = format!("HEAD:{rel_path}");
    let output = Command::new("git")
        .args(["-C", path_str, "show", &spec])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git show {spec} failed with code {:?}", output.status.code())
        } else {
            stderr
        });
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{rel_path} at HEAD is not UTF-8"))
}

/// message verbatim so the user sees exactly what blocked them.
pub fn commit_all_tracked(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    let path_str = root.to_str().ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let output = Command::new("git")
        .args(["-C", path_str, "commit", "-am", message])
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

/// Stage a single path. Maps the user's Source Control "+" click onto
/// `git add -- <path>`, which handles Modified, Deleted, Untracked, and
/// Conflicted (merge-resolved) entries uniformly. Errors propagate
/// verbatim so the caller can surface them in the status bar.
pub fn stage_path(root: &Path, rel_path: &str) -> Result<(), String> {
    let path_str = root.to_str().ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let output = Command::new("git")
        .args(["-C", path_str, "add", "--", rel_path])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git add failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// Discard a single path. For tracked entries this restores the working
/// tree to HEAD via `git checkout HEAD -- <path>`; for Untracked entries
/// the file (or directory) is removed from disk. Destructive: callers
/// MUST confirm with the user before invoking — the Source Control panel
/// shows a Y/N modal before reaching this function.
pub fn discard_path(root: &Path, rel_path: &str, untracked: bool) -> Result<(), String> {
    if untracked {
        let abs = root.join(rel_path);
        if abs.is_dir() {
            std::fs::remove_dir_all(&abs)
                .map_err(|e| format!("failed to remove {}: {e}", abs.display()))
        } else {
            std::fs::remove_file(&abs)
                .map_err(|e| format!("failed to remove {}: {e}", abs.display()))
        }
    } else {
        let path_str = root.to_str().ok_or_else(|| "non-utf8 workspace path".to_string())?;
        let output = Command::new("git")
            .args(["-C", path_str, "checkout", "HEAD", "--", rel_path])
            .output()
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            Err(if msg.is_empty() {
                format!("git checkout failed with code {:?}", output.status.code())
            } else {
                msg
            })
        }
    }
}

/// One row in the welcome-screen recent-commits panel: the short hash, a
/// human-readable relative date (e.g. "2 hours ago"), and the commit
/// subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub hash: String,
    pub full_hash: String,
    pub when: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentCommits {
    pub remote: Option<String>,
    pub commits: Vec<CommitInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitApiProvider {
    Bitbucket,
    GitHub,
    Codeberg,
}

impl CommitApiProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bitbucket => "Bitbucket",
            Self::GitHub => "GitHub",
            Self::Codeberg => "Codeberg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitApiEndpoint {
    pub provider: CommitApiProvider,
    pub url: String,
}

const DEFAULT_CROFT_REPOSITORY_REMOTE: &str = "ssh://git@codeberg.org/vitali87/croft.git";

/// Why the welcome panel's commit list is empty. Used by the UI to phrase
/// the status bar honestly — the panel itself only ever shows commits from
/// a *successful, current* fetch. Stale or cached commits are deliberately
/// not an option: this is a high-velocity project and an out-of-date
/// "Recent" list is misinformation, strictly worse than an empty one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RecentCommitsError {
    #[default]
    None,
    /// `git clone` / `git log` exited non-zero (transport failure, host
    /// down, repo moved, malformed git output).
    Network,
    /// The repository remote is unset or doesn't resolve to an HTTPS URL.
    NoEndpoint,
}

pub fn fetch_croft_recent_commits_full(
    timeout: std::time::Duration,
) -> (RecentCommits, RecentCommitsError) {
    let remote = croft_repository_remote();
    let Some(https_url) = remote.as_deref().and_then(https_clone_url_for_remote) else {
        return (
            RecentCommits { remote, commits: Vec::new() },
            RecentCommitsError::NoEndpoint,
        );
    };
    let now = current_unix_seconds();
    match fetch_recent_commits_via_clone(&https_url, 5, timeout) {
        Ok(commits) => (
            RecentCommits {
                remote,
                commits: commits
                    .into_iter()
                    .map(|c| commit_info_from_log(c, now))
                    .collect(),
            },
            RecentCommitsError::None,
        ),
        Err(_) => (
            RecentCommits { remote, commits: Vec::new() },
            RecentCommitsError::Network,
        ),
    }
}

/// One row from `git log --pretty=...`. `committer_unix` is seconds since
/// epoch (committer date, %ct), used to compute the human-readable
/// "X hours ago" string at render time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLogRow {
    pub short_hash: String,
    pub full_hash: String,
    pub committer_unix: i64,
    pub subject: String,
}

fn commit_info_from_log(row: GitLogRow, now: i64) -> CommitInfo {
    CommitInfo {
        hash: row.short_hash,
        full_hash: row.full_hash,
        when: humanize_age(now.saturating_sub(row.committer_unix)),
        subject: row.subject,
    }
}

/// Convert SSH or HTTPS remote refs to a clone-able HTTPS URL. Bitbucket
/// and GitHub both accept the `https://<host>/<owner>/<repo>.git` form
/// for anonymous public clones.
pub fn https_clone_url_for_remote(remote: &str) -> Option<String> {
    let normalized = normalize_remote_reference(remote)?;
    Some(format!("{normalized}.git"))
}

fn fetch_recent_commits_via_clone(
    https_url: &str,
    depth: u32,
    timeout: std::time::Duration,
) -> std::io::Result<Vec<GitLogRow>> {
    let staging = unique_staging_dir()?;
    let _guard = TempDirGuard(staging.clone());
    // --bare: no working tree
    // --depth: shallow, only the most recent commits
    // --filter=blob:none: skip file contents (we only need commit metadata)
    // --no-tags: skip tag refs
    // --quiet: silence progress output
    let timeout_secs = timeout.as_secs().max(1).to_string();
    let clone = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", &timeout_secs)
        .args([
            "clone",
            "--bare",
            "--no-tags",
            "--filter=blob:none",
            "--quiet",
            "--depth",
        ])
        .arg(depth.to_string())
        .arg(https_url)
        .arg(&staging)
        .output()?;
    if !clone.status.success() {
        return Err(std::io::Error::other(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr).trim()
        )));
    }
    let log = Command::new("git")
        .arg("-C")
        .arg(&staging)
        .args([
            "log",
            "--no-merges",
            "--pretty=format:%h%x09%H%x09%ct%x09%s",
            "-n",
        ])
        .arg(depth.to_string())
        .output()?;
    if !log.status.success() {
        return Err(std::io::Error::other(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&log.stderr).trim()
        )));
    }
    parse_git_log_lines(&String::from_utf8_lossy(&log.stdout))
        .ok_or_else(|| std::io::Error::other("git log produced unparseable output"))
}

pub fn parse_git_log_lines(out: &str) -> Option<Vec<GitLogRow>> {
    let mut rows = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let short_hash = parts.next()?.to_string();
        let full_hash = parts.next()?.to_string();
        let unix: i64 = parts.next()?.parse().ok()?;
        let subject = parts.next().unwrap_or("").to_string();
        rows.push(GitLogRow {
            short_hash,
            full_hash,
            committer_unix: unix,
            subject,
        });
    }
    Some(rows)
}

fn unique_staging_dir() -> std::io::Result<std::path::PathBuf> {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("croft-recent-{pid}-{nanos}"));
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
    }
    Ok(path)
}

struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn croft_repository_remote() -> Option<String> {
    let raw = std::env::var("CROFT_REPOSITORY_REMOTE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            option_env!("CROFT_REPOSITORY_REMOTE")
                .filter(|s| !s.trim().is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| DEFAULT_CROFT_REPOSITORY_REMOTE.to_string());
    normalize_remote_reference(&raw)
}

pub fn normalize_remote_reference(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/');
    if remote.is_empty() {
        return None;
    }
    if let Some(rest) = remote.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return normalize_host_path(host, path);
    }
    if let Some(rest) = remote.strip_prefix("ssh://git@") {
        let (host, path) = rest.split_once('/')?;
        return normalize_host_path(host, path);
    }
    if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
    {
        let (host, path) = rest.split_once('/')?;
        return normalize_host_path(host, path);
    }
    if let Some((host, path)) = remote.split_once('/') {
        return normalize_host_path(host, path);
    }
    None
}

fn normalize_host_path(host: &str, path: &str) -> Option<String> {
    let host = host.trim();
    let path = trim_git_suffix(path.trim().trim_start_matches('/'));
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

fn trim_git_suffix(path: &str) -> String {
    path.trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'))
        .to_string()
}

pub fn commits_api_endpoint_for_remote(remote: &str) -> Option<CommitApiEndpoint> {
    let normalized = normalize_remote_reference(remote)?;
    let rest = normalized.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let mut parts = path.split('/').filter(|s| !s.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    match host {
        "bitbucket.org" => Some(CommitApiEndpoint {
            provider: CommitApiProvider::Bitbucket,
            url: format!(
                "https://api.bitbucket.org/2.0/repositories/{owner}/{repo}/commits?pagelen=5"
            ),
        }),
        "github.com" => Some(CommitApiEndpoint {
            provider: CommitApiProvider::GitHub,
            url: format!("https://api.github.com/repos/{owner}/{repo}/commits?per_page=5"),
        }),
        "codeberg.org" => Some(CommitApiEndpoint {
            provider: CommitApiProvider::Codeberg,
            url: format!("https://codeberg.org/api/v1/repos/{owner}/{repo}/commits?limit=5"),
        }),
        _ => None,
    }
}

pub fn commit_api_provider_for_remote(remote: &str) -> Option<CommitApiProvider> {
    commits_api_endpoint_for_remote(remote).map(|e| e.provider)
}

pub fn commit_url_for_remote(remote: &str, hash: &str) -> Option<String> {
    let normalized = normalize_remote_reference(remote)?;
    let provider = commit_api_provider_for_remote(&normalized)?;
    let hash = hash.trim();
    if hash.is_empty() {
        return None;
    }
    match provider {
        CommitApiProvider::Bitbucket => Some(format!("{normalized}/commits/{hash}")),
        CommitApiProvider::GitHub => Some(format!("{normalized}/commit/{hash}")),
        CommitApiProvider::Codeberg => Some(format!("{normalized}/commit/{hash}")),
    }
}

/// Pure function: takes the raw Bitbucket JSON body plus the current unix
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

fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `git rev-list --left-right --count HEAD...@{u}` returns "<ahead>\t<behind>".
pub fn parse_ahead_behind(out: &str) -> (usize, usize) {
    let mut parts = out.split_whitespace();
    let a = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (a, b)
}

/// Unit of work the App posts to the background git worker. Coalesced
/// inside the loop so a burst of clicks / FS-watcher ticks turns into a
/// single shell-out instead of N. `SetRoot` rebinds the worker's working
/// directory in-place — used by `App::change_workspace_root` so a Make
/// Root that lands inside a git repo can flip the Source Control panel
/// out of its no-repo empty state without recreating the worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRequest {
    Status,
    Changes,
    StatusAndChanges,
    SetRoot(PathBuf),
}

impl GitRequest {
    /// Merge two pending *query* requests into the strongest one.
    /// `StatusAndChanges` dominates everything; mixing `Status` and
    /// `Changes` collapses to `StatusAndChanges`; same+same is the same.
    /// `SetRoot` is never a query and the worker drains it inline before
    /// merging, so it must not appear here.
    pub fn merge(self, other: Self) -> Self {
        use GitRequest::*;
        match (self, other) {
            (SetRoot(_), _) | (_, SetRoot(_)) => {
                unreachable!("SetRoot is drained inline before merge; it cannot reach the query merge")
            }
            (StatusAndChanges, _) | (_, StatusAndChanges) => StatusAndChanges,
            (Status, Changes) | (Changes, Status) => StatusAndChanges,
            (Status, Status) => Status,
            (Changes, Changes) => Changes,
        }
    }
}

/// Result the worker ships back. The variants mirror `GitRequest` 1:1
/// so the App can route each response to the right consumer without an
/// extra request-id channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitResponse {
    Status(GitStatus),
    Changes(Vec<ChangeEntry>),
    StatusAndChanges(GitStatus, Vec<ChangeEntry>),
}

/// Background worker loop. Reads `GitRequest`s from `rx`, coalesces a
/// burst of pending requests into a single shell-out, runs `query` /
/// `query_changes` off the UI thread, and ships the result back via
/// `tx`. Terminates cleanly when either channel closes.
///
/// Croft was rewritten to Rust for raw input latency. `git status` on a
/// fresh shell adds 15-50 ms (process spawn + scanning the tree) — fine
/// off-thread, never on the hot path of a sidebar-icon click. The same
/// channel gate is used by both the click path (`refresh_source_control`)
/// and the FS-watcher tick (`refresh_git_status_debounced`).
pub fn git_worker_loop(
    initial_root: PathBuf,
    rx: std::sync::mpsc::Receiver<GitRequest>,
    tx: std::sync::mpsc::Sender<GitResponse>,
) {
    let mut root = initial_root;
    while let Ok(first) = rx.recv() {
        let mut pending: Option<GitRequest> = match first {
            GitRequest::SetRoot(p) => {
                root = p;
                None
            }
            other => Some(other),
        };
        while let Ok(newer) = rx.try_recv() {
            match newer {
                GitRequest::SetRoot(p) => {
                    root = p;
                }
                other => {
                    pending = Some(match pending {
                        Some(prev) => prev.merge(other),
                        None => other,
                    });
                }
            }
        }
        let Some(req) = pending else {
            continue;
        };
        let resp = match req {
            GitRequest::Status => GitResponse::Status(query(&root)),
            GitRequest::Changes => GitResponse::Changes(query_changes(&root)),
            GitRequest::StatusAndChanges => {
                let s = query(&root);
                let c = query_changes(&root);
                GitResponse::StatusAndChanges(s, c)
            }
            GitRequest::SetRoot(_) => unreachable!("SetRoot was drained inline"),
        };
        if tx.send(resp).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_branch_handles_typical_output() {
        assert_eq!(parse_branch("main\n".into()), Some("main".to_string()));
        assert_eq!(
            parse_branch("feature/cool-thing\n".into()),
            Some("feature/cool-thing".to_string())
        );
    }

    #[test]
    fn parse_branch_treats_empty_as_none() {
        assert_eq!(parse_branch(String::new()), None);
        assert_eq!(parse_branch("   \n".into()), None);
    }

    #[test]
    fn parse_porcelain_dirty_empty_means_clean() {
        assert!(!parse_porcelain_dirty(""));
        assert!(!parse_porcelain_dirty("\n"));
        assert!(!parse_porcelain_dirty("   "));
    }

    #[test]
    fn parse_porcelain_dirty_any_line_means_dirty() {
        assert!(parse_porcelain_dirty(" M src/app.rs\n"));
        assert!(parse_porcelain_dirty("?? new.txt\n"));
        assert!(parse_porcelain_dirty("MM Cargo.toml\n M src/lib.rs\n"));
    }

    #[test]
    fn parse_ahead_behind_typical() {
        assert_eq!(parse_ahead_behind("3\t2"), (3, 2));
        assert_eq!(parse_ahead_behind("0\t0"), (0, 0));
        assert_eq!(parse_ahead_behind("12\t0\n"), (12, 0));
    }

    #[test]
    fn parse_ahead_behind_falls_back_to_zero_on_garbage() {
        assert_eq!(parse_ahead_behind(""), (0, 0));
        assert_eq!(parse_ahead_behind("?"), (0, 0));
    }

    #[test]
    fn query_outside_a_git_repo_returns_in_repo_false() {
        let tmp = TempDir::new().unwrap();
        let s = query(tmp.path());
        assert!(!s.in_repo);
        assert!(s.branch.is_none());
        assert!(!s.dirty);
    }

    #[test]
    fn query_in_a_fresh_repo_finds_branch_and_clean() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        // Init the repo non-interactively.
        let _ = Command::new("git").args(["-C"]).arg(p).args(["init", "-q", "-b", "main"]).output();
        // Empty repo: HEAD points at unborn branch "main", but symbolic-ref
        // still works.
        let s = query(p);
        assert!(s.in_repo);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(!s.dirty);
    }

    #[test]
    fn query_reports_dirty_after_a_file_appears() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["init", "-q", "-b", "main"]).output();
        // Make a working-tree change.
        std::fs::write(p.join("hello.txt"), "hi").unwrap();
        let s = query(p);
        assert!(s.in_repo);
        assert!(s.dirty);
    }

    #[test]
    fn humanize_seconds_minutes_hours_days() {
        assert_eq!(humanize_age(0), "0 seconds ago");
        assert_eq!(humanize_age(45), "45 seconds ago");
        assert_eq!(humanize_age(60), "1 minute ago");
        assert_eq!(humanize_age(120), "2 minutes ago");
        assert_eq!(humanize_age(3600), "1 hour ago");
        assert_eq!(humanize_age(7200), "2 hours ago");
        assert_eq!(humanize_age(86_400), "1 day ago");
        assert_eq!(humanize_age(2 * 86_400), "2 days ago");
    }

    #[test]
    fn humanize_clamps_negatives_to_zero() {
        assert_eq!(humanize_age(-100), "0 seconds ago");
    }

    #[test]
    fn normalize_remote_reference_handles_bitbucket_ssh() {
        assert_eq!(
            normalize_remote_reference("git@bitbucket.org:vitali_avagyan/croft.git"),
            Some("https://bitbucket.org/vitali_avagyan/croft".to_string())
        );
    }

    #[test]
    fn normalize_remote_reference_handles_github_https() {
        assert_eq!(
            normalize_remote_reference("https://github.com/example/croft.git"),
            Some("https://github.com/example/croft".to_string())
        );
    }

    #[test]
    fn normalize_remote_reference_handles_ssh_url() {
        assert_eq!(
            normalize_remote_reference("ssh://git@github.com/example/croft.git"),
            Some("https://github.com/example/croft".to_string())
        );
    }

    #[test]
    fn commits_api_endpoint_for_remote_supports_bitbucket_and_github() {
        let bitbucket =
            commits_api_endpoint_for_remote("https://bitbucket.org/vitali_avagyan/croft")
                .unwrap();
        assert_eq!(bitbucket.provider, CommitApiProvider::Bitbucket);
        assert_eq!(
            bitbucket.url,
            "https://api.bitbucket.org/2.0/repositories/vitali_avagyan/croft/commits?pagelen=5"
        );

        let github = commits_api_endpoint_for_remote("git@github.com:example/croft.git").unwrap();
        assert_eq!(github.provider, CommitApiProvider::GitHub);
        assert_eq!(github.url, "https://api.github.com/repos/example/croft/commits?per_page=5");
    }

    #[test]
    fn commits_api_endpoint_for_remote_returns_none_for_unknown_host() {
        assert!(commits_api_endpoint_for_remote("https://gitlab.com/example/croft").is_none());
    }

    /// Codeberg is Gitea-based; the welcome panel needs to recognise it both
    /// for the "Recent commits via Codeberg" badge and so the commit-page
    /// URL builder routes through `/commit/<hash>` (Gitea), not Bitbucket's
    /// `/commits/<hash>` plural form.
    #[test]
    fn commits_api_endpoint_for_remote_supports_codeberg_ssh_url() {
        let endpoint =
            commits_api_endpoint_for_remote("ssh://git@codeberg.org/vitali87/croft.git")
                .unwrap();
        assert_eq!(endpoint.provider, CommitApiProvider::Codeberg);
        assert_eq!(
            endpoint.url,
            "https://codeberg.org/api/v1/repos/vitali87/croft/commits?limit=5"
        );
    }

    #[test]
    fn commits_api_endpoint_for_remote_supports_codeberg_https() {
        let endpoint =
            commits_api_endpoint_for_remote("https://codeberg.org/vitali87/croft").unwrap();
        assert_eq!(endpoint.provider, CommitApiProvider::Codeberg);
    }

    #[test]
    fn commit_url_for_codeberg_uses_singular_commit_path() {
        assert_eq!(
            commit_url_for_remote("ssh://git@codeberg.org/vitali87/croft.git", "abc123"),
            Some("https://codeberg.org/vitali87/croft/commit/abc123".to_string())
        );
    }

    #[test]
    fn commit_api_provider_label_includes_codeberg() {
        assert_eq!(CommitApiProvider::Codeberg.label(), "Codeberg");
    }

    /// The default repo the welcome panel pulls commits from must be the
    /// Codeberg upstream now that origin moved off Bitbucket; otherwise the
    /// welcome list goes stale because new pushes only land on Codeberg.
    #[test]
    fn croft_repository_remote_default_points_at_codeberg() {
        // The const is private; inspect the default behavior end-to-end.
        // Clear any local override so the binary's compile-time default wins.
        let prev = std::env::var("CROFT_REPOSITORY_REMOTE").ok();
        // SAFETY: tests in this module run sequentially within one process.
        unsafe {
            std::env::remove_var("CROFT_REPOSITORY_REMOTE");
        }
        let resolved = croft_repository_remote();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CROFT_REPOSITORY_REMOTE", v),
                None => std::env::remove_var("CROFT_REPOSITORY_REMOTE"),
            }
        }
        assert_eq!(
            resolved,
            Some("https://codeberg.org/vitali87/croft".to_string())
        );
    }

    #[test]
    fn commit_url_for_remote_builds_provider_specific_urls() {
        assert_eq!(
            commit_url_for_remote("git@bitbucket.org:vitali_avagyan/croft.git", "abc123"),
            Some("https://bitbucket.org/vitali_avagyan/croft/commits/abc123".to_string())
        );
        assert_eq!(
            commit_url_for_remote("https://github.com/example/croft.git", "abc123"),
            Some("https://github.com/example/croft/commit/abc123".to_string())
        );
    }

    #[test]
    fn parse_git_log_lines_handles_typical_output() {
        let out = "abc1234\tabc1234fffffffffffffffffffffffffffffffffff\t1762348800\tfeat: do thing\n\
                   def5678\tdef5678eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\t1762262400\tfix: another\n";
        let rows = parse_git_log_lines(out).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].short_hash, "abc1234");
        assert_eq!(rows[0].full_hash, "abc1234fffffffffffffffffffffffffffffffffff");
        assert_eq!(rows[0].committer_unix, 1762348800);
        assert_eq!(rows[0].subject, "feat: do thing");
        assert_eq!(rows[1].subject, "fix: another");
    }

    #[test]
    fn parse_git_log_lines_keeps_tabs_inside_subject() {
        // splitn(4, '\t') means tabs inside the subject body survive.
        let out = "h1\thash1\t100\tsubject\twith\ttabs\n";
        let rows = parse_git_log_lines(out).unwrap();
        assert_eq!(rows[0].subject, "subject\twith\ttabs");
    }

    #[test]
    fn parse_git_log_lines_skips_blank_lines() {
        let out = "\nh1\thash1\t100\tsubject\n\n";
        let rows = parse_git_log_lines(out).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn parse_git_log_lines_returns_none_on_malformed_unix_time() {
        let out = "h1\thash1\tnotanumber\tsubject\n";
        assert!(parse_git_log_lines(out).is_none());
    }

    #[test]
    fn https_clone_url_for_bitbucket_ssh_remote() {
        let url = https_clone_url_for_remote("git@bitbucket.org:vitali_avagyan/croft.git").unwrap();
        assert_eq!(url, "https://bitbucket.org/vitali_avagyan/croft.git");
    }

    #[test]
    fn https_clone_url_for_https_remote_already_normalized() {
        let url = https_clone_url_for_remote("https://github.com/owner/repo").unwrap();
        assert_eq!(url, "https://github.com/owner/repo.git");
    }

    #[test]
    fn fetch_recent_commits_via_clone_returns_real_commits_for_local_repo() {
        // Clone-from-local works through the same code path; a tempdir
        // upstream avoids hitting the network in tests but exercises the
        // git + parse pipeline end-to-end.
        let upstream = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .arg(upstream.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"]).arg(upstream.path())
            .args(["config", "user.email", "test@example.com"])
            .status().unwrap();
        Command::new("git")
            .args(["-C"]).arg(upstream.path())
            .args(["config", "user.name", "test"])
            .status().unwrap();
        std::fs::write(upstream.path().join("a.txt"), "1").unwrap();
        Command::new("git").args(["-C"]).arg(upstream.path()).args(["add", "."]).status().unwrap();
        Command::new("git").args(["-C"]).arg(upstream.path())
            .args(["commit", "-m", "first commit", "--quiet"]).status().unwrap();
        std::fs::write(upstream.path().join("a.txt"), "2").unwrap();
        Command::new("git").args(["-C"]).arg(upstream.path()).args(["add", "."]).status().unwrap();
        Command::new("git").args(["-C"]).arg(upstream.path())
            .args(["commit", "-m", "second commit", "--quiet"]).status().unwrap();
        let url = format!("file://{}", upstream.path().display());
        let rows = fetch_recent_commits_via_clone(&url, 5, std::time::Duration::from_secs(10))
            .expect("local clone must succeed");
        assert!(rows.len() >= 2);
        assert_eq!(rows[0].subject, "second commit");
        assert_eq!(rows[1].subject, "first commit");
    }

    #[test]
    fn parse_porcelain_changes_handles_unstaged_modified() {
        let entries = parse_porcelain_changes(" M src/app.rs\n");
        assert_eq!(entries, vec![ChangeEntry {
            path: "src/app.rs".to_string(),
            kind: ChangeKind::Modified,
        }]);
    }

    #[test]
    fn parse_porcelain_changes_handles_staged_modified() {
        let entries = parse_porcelain_changes("M  src/app.rs\n");
        assert_eq!(entries, vec![ChangeEntry {
            path: "src/app.rs".to_string(),
            kind: ChangeKind::StagedModified,
        }]);
    }

    #[test]
    fn parse_porcelain_changes_emits_two_entries_for_partially_staged() {
        // MM: staged content then further modified in worktree.
        let entries = parse_porcelain_changes("MM src/app.rs\n");
        assert_eq!(
            entries,
            vec![
                ChangeEntry { path: "src/app.rs".to_string(), kind: ChangeKind::StagedModified },
                ChangeEntry { path: "src/app.rs".to_string(), kind: ChangeKind::Modified },
            ]
        );
    }

    #[test]
    fn parse_porcelain_changes_handles_untracked() {
        let entries = parse_porcelain_changes("?? scratch.txt\n");
        assert_eq!(entries, vec![ChangeEntry {
            path: "scratch.txt".to_string(),
            kind: ChangeKind::Untracked,
        }]);
    }

    #[test]
    fn parse_porcelain_changes_handles_rename_takes_destination_path() {
        let entries = parse_porcelain_changes("R  old.txt -> new.txt\n");
        assert_eq!(entries, vec![ChangeEntry {
            path: "new.txt".to_string(),
            kind: ChangeKind::StagedRenamed,
        }]);
    }

    #[test]
    fn parse_porcelain_changes_handles_conflict() {
        let entries = parse_porcelain_changes("UU merge.txt\n");
        assert_eq!(entries, vec![ChangeEntry {
            path: "merge.txt".to_string(),
            kind: ChangeKind::Conflicted,
        }]);
    }

    #[test]
    fn change_kind_section_groups_correctly() {
        assert_eq!(ChangeKind::StagedAdded.section(), ChangeSection::Staged);
        assert_eq!(ChangeKind::Modified.section(), ChangeSection::Changes);
        assert_eq!(ChangeKind::Untracked.section(), ChangeSection::Untracked);
        assert_eq!(ChangeKind::Conflicted.section(), ChangeSection::Conflicts);
    }

    #[test]
    fn commit_all_tracked_rejects_empty_message() {
        let tmp = TempDir::new().unwrap();
        assert!(commit_all_tracked(tmp.path(), "").is_err());
        assert!(commit_all_tracked(tmp.path(), "   \n").is_err());
    }

    #[test]
    fn commit_all_tracked_creates_a_commit_in_a_real_repo() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["init", "-q", "-b", "main"]).output();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.email", "a@b"]).status();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.name", "a"]).status();
        std::fs::write(p.join("hello.txt"), "hi").unwrap();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["add", "."]).status();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["commit", "-m", "init", "--quiet"]).status();
        std::fs::write(p.join("hello.txt"), "hi v2").unwrap();
        let summary = commit_all_tracked(p, "second commit").expect("commit should succeed");
        assert!(summary.contains("second commit") || summary.contains("main"), "summary was: {summary}");
        assert!(query_changes(p).is_empty(), "post-commit working tree should be clean");
    }

    #[test]
    fn git_request_merge_collapses_status_and_changes_into_status_and_changes() {
        assert_eq!(
            GitRequest::Status.merge(GitRequest::Changes),
            GitRequest::StatusAndChanges,
        );
        assert_eq!(
            GitRequest::Changes.merge(GitRequest::Status),
            GitRequest::StatusAndChanges,
        );
        assert_eq!(
            GitRequest::Status.merge(GitRequest::Status),
            GitRequest::Status,
        );
        assert_eq!(
            GitRequest::Changes.merge(GitRequest::Changes),
            GitRequest::Changes,
        );
        assert_eq!(
            GitRequest::StatusAndChanges.merge(GitRequest::Changes),
            GitRequest::StatusAndChanges,
        );
    }

    #[test]
    fn git_worker_loop_processes_a_changes_request_and_returns_entries() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["init", "-q", "-b", "main"]).output();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.email", "a@b"]).status();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.name", "a"]).status();
        std::fs::write(p.join("untracked.txt"), "x").unwrap();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<GitResponse>();
        let root = p.to_path_buf();
        let join = std::thread::spawn(move || git_worker_loop(root, req_rx, resp_tx));
        req_tx.send(GitRequest::Changes).unwrap();
        let resp = resp_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker must reply within 10s");
        match resp {
            GitResponse::Changes(entries) => {
                assert_eq!(entries.len(), 1, "expected 1 untracked entry, got {entries:?}");
                assert_eq!(entries[0].kind, ChangeKind::Untracked);
            }
            other => panic!("expected Changes, got {other:?}"),
        }
        drop(req_tx);
        join.join().unwrap();
    }

    #[test]
    fn git_worker_loop_set_root_redirects_subsequent_query_to_the_new_root() {
        let tmp_no_repo = TempDir::new().unwrap();
        let tmp_repo = TempDir::new().unwrap();
        let p_repo = tmp_repo.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p_repo)
            .args(["init", "-q", "-b", "feature-x"])
            .output();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p_repo)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p_repo)
            .args(["config", "user.name", "a"])
            .status();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<GitResponse>();
        let initial_root = tmp_no_repo.path().to_path_buf();
        let join = std::thread::spawn(move || git_worker_loop(initial_root, req_rx, resp_tx));
        req_tx
            .send(GitRequest::SetRoot(p_repo.to_path_buf()))
            .unwrap();
        req_tx.send(GitRequest::Status).unwrap();
        let resp = resp_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker must reply within 10s after SetRoot+Status");
        match resp {
            GitResponse::Status(status) => {
                assert!(
                    status.in_repo,
                    "after SetRoot to a real git repo the next Status must report in_repo=true (the bug was that SetRoot didn't exist, so queries kept hitting the stale root captured at thread spawn)"
                );
                assert_eq!(status.branch.as_deref(), Some("feature-x"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
        drop(req_tx);
        join.join().unwrap();
    }

    #[test]
    fn git_worker_loop_coalesces_a_burst_into_one_status_and_changes_response() {
        // Two pending requests (Status + Changes) that arrive faster than
        // the worker wakes up must collapse to a single
        // StatusAndChanges response (the coalesce drains rx via
        // try_recv before the second .recv()). Without the merge, the
        // worker would shell out twice for what the App treats as one
        // logical refresh.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["init", "-q", "-b", "main"]).output();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.email", "a@b"]).status();
        let _ = Command::new("git").args(["-C"]).arg(p).args(["config", "user.name", "a"]).status();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<GitResponse>();
        let root = p.to_path_buf();
        // Queue two requests BEFORE the worker starts so the first
        // recv() succeeds and the try_recv coalesce sees the second one.
        req_tx.send(GitRequest::Status).unwrap();
        req_tx.send(GitRequest::Changes).unwrap();
        let join = std::thread::spawn(move || git_worker_loop(root, req_rx, resp_tx));
        let first = resp_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker must reply within 10s");
        assert!(
            matches!(first, GitResponse::StatusAndChanges(_, _)),
            "burst must coalesce into one StatusAndChanges response, got {first:?}",
        );
        // No second response — the two pending requests merged.
        let second = resp_rx.recv_timeout(std::time::Duration::from_millis(200));
        assert!(
            second.is_err(),
            "coalesce must produce exactly one response per burst, got an extra {second:?}",
        );
        drop(req_tx);
        join.join().unwrap();
    }
}
