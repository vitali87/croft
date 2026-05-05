use std::path::Path;
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
}

impl CommitApiProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bitbucket => "Bitbucket",
            Self::GitHub => "GitHub",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitApiEndpoint {
    pub provider: CommitApiProvider,
    pub url: String,
}

const DEFAULT_CROFT_REPOSITORY_REMOTE: &str = "git@bitbucket.org:vitali_avagyan/croft.git";

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

/// Live-fetch the latest 5 commits for the croft repository using the
/// anonymous git smart-HTTP protocol. Bypasses the per-IP REST-API rate
/// limit (60/h on Bitbucket Cloud) by going through the same endpoint
/// `git clone` uses, which is provisioned for very different traffic and
/// works the same for every developer regardless of VPN, NAT, or shared
/// egress IP. Synchronous — callers should run this off the UI thread.
pub fn fetch_croft_recent_commits(timeout: std::time::Duration) -> RecentCommits {
    fetch_croft_recent_commits_full(timeout).0
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
}
