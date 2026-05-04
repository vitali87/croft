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

/// Live-fetch the latest 5 croft commits from the repository remote. Synchronous;
/// callers should run this off the UI thread (see `App::spawn_recent_commits_fetch`).
/// Returns only the remote reference on any fetch error (network down, API
/// change, unsupported provider).
pub fn fetch_croft_recent_commits(timeout: std::time::Duration) -> RecentCommits {
    let remote = croft_repository_remote();
    let Some(endpoint) = remote.as_deref().and_then(commits_api_endpoint_for_remote) else {
        return RecentCommits { remote, commits: Vec::new() };
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .build();
    let resp = match agent
        .get(&endpoint.url)
        .set("User-Agent", "croft")
        .call()
    {
        Ok(r) => r,
        Err(_) => return RecentCommits { remote, commits: Vec::new() },
    };
    let body = match resp.into_string() {
        Ok(s) => s,
        Err(_) => return RecentCommits { remote, commits: Vec::new() },
    };
    let now = current_unix_seconds();
    let commits = match endpoint.provider {
        CommitApiProvider::Bitbucket => parse_bitbucket_commits_response(&body, now),
        CommitApiProvider::GitHub => parse_github_commits_response(&body, now),
    };
    RecentCommits { remote, commits }
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
/// timestamp and returns a Vec of CommitInfo. Tested in isolation.
pub fn parse_bitbucket_commits_response(body: &str, now_secs: i64) -> Vec<CommitInfo> {
    let parsed: BitbucketCommitsResponse = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    parsed
        .values
        .into_iter()
        .filter_map(|c| convert_bitbucket_commit(&c, now_secs))
        .collect()
}

pub fn parse_github_commits_response(body: &str, now_secs: i64) -> Vec<CommitInfo> {
    let parsed: Vec<GitHubCommit> = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    parsed
        .into_iter()
        .filter_map(|c| convert_github_commit(&c, now_secs))
        .collect()
}

#[derive(serde::Deserialize)]
pub struct BitbucketCommitsResponse {
    pub values: Vec<BitbucketCommit>,
}

#[derive(serde::Deserialize)]
pub struct BitbucketCommit {
    pub hash: String,
    pub date: String,
    pub message: String,
}

fn convert_bitbucket_commit(c: &BitbucketCommit, now_secs: i64) -> Option<CommitInfo> {
    let full_hash = c.hash.trim().to_string();
    let hash: String = c.hash.chars().take(7).collect();
    if hash.trim().is_empty() {
        return None;
    }
    let subject = c
        .message
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let when = match parse_rfc3339_to_unix(&c.date) {
        Some(ts) => humanize_age(now_secs.saturating_sub(ts)),
        None => c.date.clone(),
    };
    Some(CommitInfo { hash, full_hash, when, subject })
}

#[derive(serde::Deserialize)]
pub struct GitHubCommit {
    pub sha: String,
    pub commit: GitHubCommitBody,
}

#[derive(serde::Deserialize)]
pub struct GitHubCommitBody {
    pub message: String,
    pub committer: Option<GitHubCommitPerson>,
    pub author: Option<GitHubCommitPerson>,
}

#[derive(serde::Deserialize)]
pub struct GitHubCommitPerson {
    pub date: String,
}

fn convert_github_commit(c: &GitHubCommit, now_secs: i64) -> Option<CommitInfo> {
    let full_hash = c.sha.trim().to_string();
    let hash: String = c.sha.chars().take(7).collect();
    if hash.trim().is_empty() {
        return None;
    }
    let subject = c
        .commit
        .message
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let date = c
        .commit
        .committer
        .as_ref()
        .or(c.commit.author.as_ref())
        .map(|p| p.date.as_str())
        .unwrap_or("");
    let when = match parse_rfc3339_to_unix(date) {
        Some(ts) => humanize_age(now_secs.saturating_sub(ts)),
        None => date.to_string(),
    };
    Some(CommitInfo { hash, full_hash, when, subject })
}

/// Parse a date in the formats Bitbucket emits:
///   2026-05-04T16:23:45+00:00
///   2026-05-04T16:23:45.123456+00:00
///   2026-05-04T16:23:45Z
/// Returns the unix timestamp in seconds. Returns None for any malformed
/// input. Roll our own to avoid pulling chrono/time for one parse.
pub fn parse_rfc3339_to_unix(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let mo: i64 = date_parts.next()?.parse().ok()?;
    let d: i64 = date_parts.next()?.parse().ok()?;

    // Strip any trailing fractional seconds before splitting time/offset.
    let no_frac = match rest.find('.') {
        Some(dot) => {
            let after_dot = &rest[dot + 1..];
            let frac_end = after_dot
                .find(|c: char| c == 'Z' || c == '+' || c == '-')
                .unwrap_or(after_dot.len());
            let mut joined = String::with_capacity(rest.len());
            joined.push_str(&rest[..dot]);
            joined.push_str(&after_dot[frac_end..]);
            joined
        }
        None => rest.to_string(),
    };

    let (time_str, offset_secs) = if let Some(stripped) = no_frac.strip_suffix('Z') {
        (stripped.to_string(), 0i64)
    } else {
        let sign_idx = no_frac
            .rfind(|c: char| c == '+' || c == '-')?;
        let sign = if &no_frac[sign_idx..sign_idx + 1] == "+" { 1i64 } else { -1i64 };
        let off = &no_frac[sign_idx + 1..];
        let mut off_parts = off.splitn(2, ':');
        let oh: i64 = off_parts.next()?.parse().ok()?;
        let om: i64 = off_parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (no_frac[..sign_idx].to_string(), sign * (oh * 3600 + om * 60))
    };

    let mut t_parts = time_str.splitn(3, ':');
    let h: i64 = t_parts.next()?.parse().ok()?;
    let mi: i64 = t_parts.next()?.parse().ok()?;
    let se: i64 = t_parts.next()?.parse().ok()?;

    let days = days_from_civil(y, mo, d);
    let utc_secs = days * 86_400 + h * 3_600 + mi * 60 + se - offset_secs;
    Some(utc_secs)
}

/// Howard Hinnant's days_from_civil algorithm: returns the number of days
/// since 1970-01-01 for a given Gregorian (y, m, d). Public-domain.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

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
    fn rfc3339_z_suffix_parses_to_unix_epoch() {
        // 1970-01-01T00:00:00Z is the epoch.
        assert_eq!(parse_rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn rfc3339_with_offset_subtracts_offset_to_get_utc() {
        // 2026-01-01T01:00:00+01:00 == 2026-01-01T00:00:00Z
        let utc = parse_rfc3339_to_unix("2026-01-01T00:00:00Z").unwrap();
        let plus = parse_rfc3339_to_unix("2026-01-01T01:00:00+01:00").unwrap();
        assert_eq!(utc, plus);
    }

    #[test]
    fn rfc3339_with_negative_offset_adds_to_utc() {
        // 2026-01-01T00:00:00-05:00 == 2026-01-01T05:00:00Z
        let east = parse_rfc3339_to_unix("2026-01-01T05:00:00Z").unwrap();
        let west = parse_rfc3339_to_unix("2026-01-01T00:00:00-05:00").unwrap();
        assert_eq!(east, west);
    }

    #[test]
    fn rfc3339_strips_fractional_seconds() {
        let a = parse_rfc3339_to_unix("2026-05-04T16:23:45+00:00").unwrap();
        let b = parse_rfc3339_to_unix("2026-05-04T16:23:45.123456+00:00").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        assert_eq!(parse_rfc3339_to_unix(""), None);
        assert_eq!(parse_rfc3339_to_unix("not a date"), None);
        assert_eq!(parse_rfc3339_to_unix("2026-05-04"), None); // no time
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
    fn parse_bitbucket_response_extracts_first_message_line_and_short_hash() {
        let now = parse_rfc3339_to_unix("2026-05-04T16:30:00Z").unwrap();
        let body = r#"{
            "values": [
              {
                "hash": "abc1234deadbeef",
                "date": "2026-05-04T16:00:00+00:00",
                "message": "feat: live commits\n\nbody line ignored"
              },
              {
                "hash": "fedc987",
                "date": "2026-05-03T12:00:00+00:00",
                "message": "fix: parse offsets"
              }
            ]
        }"#;
        let got = parse_bitbucket_commits_response(body, now);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].hash, "abc1234");
        assert_eq!(got[0].full_hash, "abc1234deadbeef");
        assert_eq!(got[0].subject, "feat: live commits");
        assert_eq!(got[0].when, "30 minutes ago");
        assert_eq!(got[1].hash, "fedc987");
        assert_eq!(got[1].subject, "fix: parse offsets");
    }

    #[test]
    fn parse_bitbucket_response_returns_empty_on_garbage() {
        assert!(parse_bitbucket_commits_response("not json", 0).is_empty());
        assert!(parse_bitbucket_commits_response("{}", 0).is_empty());
    }

    #[test]
    fn parse_github_response_extracts_first_message_line_and_short_hash() {
        let now = parse_rfc3339_to_unix("2026-05-04T16:30:00Z").unwrap();
        let body = r#"[
          {
            "sha": "abc1234deadbeef",
            "commit": {
              "message": "feat: live commits\n\nbody line ignored",
              "committer": { "date": "2026-05-04T16:00:00Z" },
              "author": { "date": "2026-05-04T15:00:00Z" }
            }
          },
          {
            "sha": "fedc987",
            "commit": {
              "message": "fix: parse offsets",
              "committer": null,
              "author": { "date": "2026-05-03T12:00:00Z" }
            }
          }
        ]"#;
        let got = parse_github_commits_response(body, now);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].hash, "abc1234");
        assert_eq!(got[0].full_hash, "abc1234deadbeef");
        assert_eq!(got[0].subject, "feat: live commits");
        assert_eq!(got[0].when, "30 minutes ago");
        assert_eq!(got[1].hash, "fedc987");
        assert_eq!(got[1].subject, "fix: parse offsets");
    }

    #[test]
    fn parse_github_response_returns_empty_on_garbage() {
        assert!(parse_github_commits_response("not json", 0).is_empty());
        assert!(parse_github_commits_response("{}", 0).is_empty());
    }
}
