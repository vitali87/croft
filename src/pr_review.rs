//! Pull request review mode (#365): the data behind the PULL REQUEST view.
//!
//! Everything here is plain data and pure functions over `gh`'s JSON, so the
//! view, the checks refresh and the viewed-state store can be tested without
//! a network or a GitHub account. Fetching is the caller's job.

// The view that consumes this lands in the next change.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The `gh pr view --json` fields review mode reads.
pub const PR_FIELDS: &str =
    "number,title,author,headRefName,headRefOid,baseRefName,files,statusCheckRollup,url";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// `ADDED`, `MODIFIED`, `DELETED`, `RENAMED`, `COPIED`, `CHANGED`.
    pub change: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Fail,
    Pending,
    Pass,
    /// Skipped, neutral or cancelled: finished, and neither passed nor failed.
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub head_ref: String,
    pub head_oid: String,
    pub base_ref: String,
    pub files: Vec<PrFile>,
    pub checks: Vec<Check>,
}

/// The `gh` arguments that fetch the pull request `selector` names (a
/// number, or its URL) in the shape [`parse_pr`] reads.
pub fn view_args(selector: &str) -> Vec<String> {
    vec![
        String::from("pr"),
        String::from("view"),
        selector.to_string(),
        String::from("--json"),
        String::from(PR_FIELDS),
    ]
}

/// Split a whole-PR patch (`gh pr diff <n>`) into one section per file,
/// keyed by the new-side path (the old one for a deleted file).
pub fn split_diff_by_file(patch: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<(String, String, String)> = None; // (a, b, text)
    let flush = |cur: Option<(String, String, String)>, out: &mut BTreeMap<String, String>| {
        if let Some((a, b, text)) = cur {
            let deleted = text.lines().any(|l| l == "+++ /dev/null");
            out.insert(if deleted { a } else { b }, text);
        }
    };
    for line in patch.split_inclusive('\n') {
        if let Some(rest) = line.trim_end_matches('\n').strip_prefix("diff --git a/") {
            flush(current.take(), &mut out);
            let (a, b) = rest.split_once(" b/").unwrap_or((rest, rest));
            current = Some((a.to_string(), b.to_string(), String::new()));
        }
        if let Some((_, _, text)) = current.as_mut() {
            text.push_str(line);
        }
    }
    flush(current, &mut out);
    out
}

/// `gh run view <run> [--job <job>] --log-failed`: the failing steps' log.
pub fn log_args(run: u64, job: Option<u64>) -> Vec<String> {
    let mut args = vec![String::from("run"), String::from("view"), run.to_string()];
    if let Some(job) = job {
        args.push(String::from("--job"));
        args.push(job.to_string());
    }
    args.push(String::from("--log-failed"));
    args
}

/// `owner/repo#number` from the PR's URL: the viewed-store key, so marks from
/// two repositories with the same PR number never mix.
pub fn review_key(pr: &PrInfo) -> String {
    let path = pr
        .url
        .split("github.com/")
        .nth(1)
        .and_then(|rest| rest.split("/pull/").next())
        .unwrap_or("");
    format!("{path}#{}", pr.number)
}

/// What `gh pr view` should look up for a pull request as a person types
/// it: `579` or `#579` is that number in this repository, and a pasted URL
/// stays a URL (trimmed to the PR itself), so a PR from another repository
/// opens that PR rather than this repository's PR with the same number.
pub fn parse_pr_selector(input: &str) -> Option<String> {
    let t = input.trim();
    let number = |s: &str| s.parse::<u64>().ok().filter(|n| *n > 0);
    match t.split_once("/pull/") {
        Some((base, rest)) => {
            let n = number(rest.split('/').next().unwrap_or(""))?;
            Some(format!("{base}/pull/{n}"))
        }
        None => number(t.strip_prefix('#').unwrap_or(t)).map(|n| n.to_string()),
    }
}

/// `gh pr list` for the Review Pull Request picker: the open PRs, newest
/// first, with what a row shows.
pub fn list_args() -> Vec<String> {
    [
        "pr",
        "list",
        "--limit",
        "50",
        "--json",
        "number,title,author,url,isDraft",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// One open pull request as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrListItem {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub draft: bool,
}

/// Parse `gh pr list --json number,title,author,url,isDraft`.
pub fn parse_pr_list(json: &str) -> Result<Vec<PrListItem>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let items = v.as_array().ok_or("gh pr list did not return a list")?;
    Ok(items
        .iter()
        .filter_map(|it| {
            Some(PrListItem {
                number: it.get("number")?.as_u64()?,
                title: it
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                author: it
                    .pointer("/author/login")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: it
                    .get("url")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                draft: it.get("isDraft").and_then(|x| x.as_bool()).unwrap_or(false),
            })
        })
        .collect())
}

/// How long one `gh` call may take before it is killed and reported: a
/// slow network or a large log must not hang the review for good.
pub const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `gh` (`program`) with `args` in `cwd` and return its stdout, or its
/// stderr (or why it could not run, or that it timed out) as the error.
/// Blocking: the app calls it on a worker thread.
pub fn run_gh(
    program: &str,
    args: &[String],
    cwd: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    // Drained while it runs: a log larger than a pipe buffer would
    // otherwise block gh on its write until the deadline.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took longer than {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(format!("could not wait on {program}: {e}")),
        }
    };
    let text = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        String::from_utf8_lossy(&rx.recv().unwrap_or_default()).into_owned()
    };
    if status.success() {
        Ok(text(stdout))
    } else {
        Err(text(stderr).trim().to_string())
    }
}

static STARTUP_PR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// `croft pr <n>` records the selector here before the app starts.
pub fn set_startup_pr(selector: String) {
    if let Ok(mut slot) = STARTUP_PR.lock() {
        *slot = Some(selector);
    }
}

/// The app takes it once, after its first frame.
pub fn take_startup_pr() -> Option<String> {
    STARTUP_PR.lock().ok().and_then(|mut slot| slot.take())
}

/// Parse `gh pr view <n> --json PR_FIELDS`.
pub fn parse_pr(json: &str) -> Result<PrInfo, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let str_at = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let number = v
        .get("number")
        .and_then(|n| n.as_u64())
        .ok_or_else(|| String::from("no pull request number in gh's answer"))?;
    let files = v
        .get("files")
        .and_then(|f| f.as_array())
        .map(|files| {
            files
                .iter()
                .map(|f| PrFile {
                    path: f
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string(),
                    additions: f.get("additions").and_then(|n| n.as_u64()).unwrap_or(0),
                    deletions: f.get("deletions").and_then(|n| n.as_u64()).unwrap_or(0),
                    change: f
                        .get("changeType")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let checks = v
        .get("statusCheckRollup")
        .and_then(|c| c.as_array())
        .map(|checks| checks.iter().map(check_from).collect())
        .unwrap_or_default();
    Ok(PrInfo {
        number,
        title: str_at("title"),
        author: v
            .pointer("/author/login")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string(),
        url: str_at("url"),
        head_ref: str_at("headRefName"),
        head_oid: str_at("headRefOid"),
        base_ref: str_at("baseRefName"),
        files,
        checks,
    })
}

/// One `statusCheckRollup` entry. A `CheckRun` has `status` and, once done,
/// `conclusion`; a `StatusContext` (the older commit-status API, which bots
/// such as CodeRabbit use) has only `state`.
fn check_from(c: &serde_json::Value) -> Check {
    let s = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let name =
        if s("__typename") == "StatusContext" || c.get("context").is_some_and(|x| x.is_string()) {
            s("context")
        } else {
            s("name")
        }
        .to_string();
    let verdict = if s("__typename") == "StatusContext" {
        s("state")
    } else if s("status") == "COMPLETED" {
        s("conclusion")
    } else {
        "PENDING"
    };
    let state = match verdict {
        "SUCCESS" => CheckState::Pass,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
            CheckState::Fail
        }
        "SKIPPED" | "NEUTRAL" | "CANCELLED" | "STALE" => CheckState::Neutral,
        _ => CheckState::Pending,
    };
    let url = [s("detailsUrl"), s("targetUrl")]
        .into_iter()
        .find(|u| !u.is_empty())
        .map(str::to_string);
    Check { name, state, url }
}

/// The Actions run and job ids in a check's details URL
/// (`…/actions/runs/<run>/job/<job>`), for `gh run view --log`.
pub fn run_and_job(url: &str) -> Option<(u64, Option<u64>)> {
    let after = url.split("/actions/runs/").nth(1)?;
    let mut parts = after.split('/');
    let run = parts.next()?.parse().ok()?;
    let job = match (parts.next(), parts.next()) {
        (Some("job"), Some(j)) => j.parse().ok(),
        _ => None,
    };
    Some((run, job))
}

/// `3 of 12 viewed · 10 passed · 1 failed · 1 pending`, zero counts omitted.
pub fn summary(pr: &PrInfo, viewed: &BTreeSet<String>) -> String {
    let seen = pr.files.iter().filter(|f| viewed.contains(&f.path)).count();
    let mut parts = vec![format!("{seen} of {} viewed", pr.files.len())];
    let count = |st: CheckState| pr.checks.iter().filter(|c| c.state == st).count();
    for (st, word) in [
        (CheckState::Pass, "passed"),
        (CheckState::Fail, "failed"),
        (CheckState::Pending, "pending"),
    ] {
        let n = count(st);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    parts.join(" · ")
}

/// A comment waiting in a pending review (#366).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingComment {
    pub path: String,
    /// Right-side (new file) line, 1-based, as GitHub's review API takes it.
    pub line: u32,
    pub body: String,
}

/// How a review is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    pub fn api(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }
}

/// The right-side line numbers a unified diff lets a review comment on:
/// every added and context line of every hunk.
pub fn commentable_lines(patch: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let mut line: Option<u32> = None;
    for l in patch.lines() {
        if let Some(rest) = l.strip_prefix("@@") {
            // `@@ -a,b +c,d @@`: the new side starts at c.
            line = rest
                .split_whitespace()
                .find_map(|t| t.strip_prefix('+'))
                .and_then(|t| t.split(',').next())
                .and_then(|n| n.parse().ok());
            continue;
        }
        let Some(n) = line.as_mut() else {
            continue;
        };
        match l.chars().next() {
            Some('+') | Some(' ') => {
                out.insert(*n);
                *n += 1;
            }
            Some('-') | Some('\\') => {}
            _ => line = None,
        }
    }
    out
}

/// The body of `POST /repos/{o}/{r}/pulls/{n}/reviews`: one request carrying
/// the verdict and every pending comment, so they arrive as one review.
pub fn review_payload(
    commit_id: &str,
    event: ReviewEvent,
    body: &str,
    comments: &[PendingComment],
) -> serde_json::Value {
    serde_json::json!({
        "commit_id": commit_id,
        "event": event.api(),
        "body": body,
        "comments": comments
            .iter()
            .map(|c| serde_json::json!({
                "path": c.path,
                "line": c.line,
                "side": "RIGHT",
                "body": c.body,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A comment box offered for export to a pull request (#368).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBox {
    pub path: String,
    pub line: u32,
    pub body: String,
    /// Written by the navigator rather than a person.
    pub ai: bool,
}

/// Boxes on lines the diff covers become inline comments; the rest are
/// gathered into a summary paragraph, since GitHub rejects a review comment
/// on a line outside the diff.
pub fn split_export(
    boxes: &[ExportBox],
    diff: &BTreeMap<String, BTreeSet<u32>>,
) -> (Vec<PendingComment>, String) {
    let mut inline = Vec::new();
    let mut off = Vec::new();
    for b in boxes {
        let body = if b.ai {
            format!("(AI-authored) {}", b.body)
        } else {
            b.body.clone()
        };
        if diff
            .get(&b.path)
            .is_some_and(|lines| lines.contains(&b.line))
        {
            inline.push(PendingComment {
                path: b.path.clone(),
                line: b.line,
                body,
            });
        } else {
            off.push(format!("- `{}:{}`: {body}", b.path, b.line));
        }
    }
    let summary = if off.is_empty() {
        String::new()
    } else {
        format!("Comments on lines outside this diff:\n\n{}", off.join("\n"))
    };
    (inline, summary)
}

/// Which files of which pull requests the user marked viewed, kept on disk
/// so the checkboxes survive a restart. Keyed by `owner/repo#number`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewedStore {
    #[serde(default)]
    pub prs: BTreeMap<String, BTreeSet<String>>,
}

impl ViewedStore {
    /// Read the store; a missing or unreadable file is an empty store, since
    /// losing viewed marks costs a re-click and refusing to open review mode
    /// would cost the review.
    pub fn load(path: &Path) -> ViewedStore {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Flip a file's viewed mark; returns the new state.
    pub fn toggle(&mut self, pr: &str, file: &str) -> bool {
        let set = self.prs.entry(pr.to_string()).or_default();
        if set.remove(file) {
            if set.is_empty() {
                self.prs.remove(pr);
            }
            false
        } else {
            set.insert(file.to_string());
            true
        }
    }

    pub fn viewed(&self, pr: &str) -> BTreeSet<String> {
        self.prs.get(pr).cloned().unwrap_or_default()
    }

    /// Set a file's viewed mark outright, as GitHub reported it.
    pub fn set(&mut self, pr: &str, file: &str, viewed: bool) {
        if viewed {
            self.prs
                .entry(pr.to_string())
                .or_default()
                .insert(file.to_string());
        } else if let Some(set) = self.prs.get_mut(pr) {
            set.remove(file);
            if set.is_empty() {
                self.prs.remove(pr);
            }
        }
    }
}

/// A file's `viewerViewedState` on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedState {
    Viewed,
    Unviewed,
    /// Viewed once, then changed by a later push: GitHub unticks it, and
    /// so does croft.
    Dismissed,
}

impl ViewedState {
    pub fn is_viewed(self) -> bool {
        self == ViewedState::Viewed
    }
}

/// One page of a pull request's files with their viewed state, as
/// [`viewed_query_args`] asks for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedPage {
    /// The pull request's node id, which the viewed mutations take.
    pub pr_id: String,
    pub states: BTreeMap<String, ViewedState>,
    /// The cursor of the next page, when there is one.
    pub next: Option<String>,
}

/// One line, so a stub `gh` that logs its argv logs one line per call.
const VIEWED_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id files(first: 100, after: $after) { nodes { path viewerViewedState } pageInfo { hasNextPage endCursor } } } } }";

/// The `gh api graphql` arguments that read one page of the viewed state
/// of pull request `number` in `slug` (`owner/repo`), after `after`. None
/// when `slug` does not name a GitHub repository.
pub fn viewed_query_args(slug: &str, number: u64, after: Option<&str>) -> Option<Vec<String>> {
    let (owner, name) = slug.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    let mut args: Vec<String> = [
        "api",
        "graphql",
        "-f",
        &format!("query={VIEWED_QUERY}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
        "-F",
        &format!("number={number}"),
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(cursor) = after {
        args.push(String::from("-f"));
        args.push(format!("after={cursor}"));
    }
    Some(args)
}

/// Parse one [`viewed_query_args`] answer. GraphQL reports most failures
/// in an `errors` list beside partial data, so any error fails the page.
pub fn parse_viewed_page(json: &str) -> Result<ViewedPage, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if let Some(msg) = v
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
    {
        let text = msg.get("message").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("GitHub refused the query: {text}"));
    }
    let pr = v
        .pointer("/data/repository/pullRequest")
        .filter(|p| !p.is_null())
        .ok_or("GitHub did not return the pull request")?;
    let pr_id = pr
        .get("id")
        .and_then(|i| i.as_str())
        .filter(|i| !i.is_empty())
        .ok_or("GitHub did not return the pull request's id")?
        .to_string();
    let states = pr
        .pointer("/files/nodes")
        .and_then(|n| n.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let path = n.get("path")?.as_str()?.to_string();
                    let state = match n.get("viewerViewedState")?.as_str()? {
                        "VIEWED" => ViewedState::Viewed,
                        "DISMISSED" => ViewedState::Dismissed,
                        _ => ViewedState::Unviewed,
                    };
                    Some((path, state))
                })
                .collect()
        })
        .unwrap_or_default();
    let next = pr
        .pointer("/files/pageInfo")
        .filter(|p| p.get("hasNextPage").and_then(|h| h.as_bool()) == Some(true))
        .and_then(|p| p.get("endCursor"))
        .and_then(|c| c.as_str())
        .map(str::to_string);
    Ok(ViewedPage {
        pr_id,
        states,
        next,
    })
}

/// GitHub caps a pull request's file list at 3000 files: 30 pages of 100.
const VIEWED_MAX_PAGES: usize = 30;

/// Read every page of pull request `number`'s viewed state in `slug`:
/// its node id and each file's state. Blocking: the app calls it on a
/// worker thread.
pub fn fetch_viewed(
    program: &str,
    slug: &str,
    number: u64,
    cwd: &Path,
    timeout: std::time::Duration,
) -> Result<(String, BTreeMap<String, ViewedState>), String> {
    let mut states = BTreeMap::new();
    let mut after: Option<String> = None;
    for _ in 0..VIEWED_MAX_PAGES {
        let args = viewed_query_args(slug, number, after.as_deref())
            .ok_or_else(|| format!("{slug:?} is not a GitHub repository"))?;
        let page = parse_viewed_page(&run_gh(program, &args, cwd, timeout)?)?;
        states.extend(page.states);
        if page.next.is_none() {
            return Ok((page.pr_id, states));
        }
        after = page.next;
    }
    Err(String::from("too many pages of changed files"))
}

/// The `gh api graphql` arguments that mark (or, with `viewed` false,
/// unmark) `path` viewed in the pull request whose node id is `pr_id`.
pub fn mark_viewed_args(pr_id: &str, path: &str, viewed: bool) -> Vec<String> {
    let mutation = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    vec![
        String::from("api"),
        String::from("graphql"),
        String::from("-f"),
        format!(
            "query=mutation($id: ID!, $path: String!) {{ {mutation}(input: {{pullRequestId: $id, path: $path}}) {{ clientMutationId }} }}"
        ),
        String::from("-f"),
        format!("id={pr_id}"),
        String::from("-f"),
        format!("path={path}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str = r#"{
      "number": 579, "title": "feat(sarif): model", "url": "https://github.com/o/r/pull/579",
      "author": {"login": "vitali87"},
      "headRefName": "feat/x", "headRefOid": "9f100db0000000000000000000000000000000aa", "baseRefName": "main",
      "files": [
        {"path": "src/sarif/view.rs", "additions": 1048, "deletions": 0, "changeType": "ADDED"},
        {"path": "Cargo.toml", "additions": 1, "deletions": 1, "changeType": "MODIFIED"}
      ],
      "statusCheckRollup": [
        {"__typename": "CheckRun", "name": "clippy + tests", "status": "COMPLETED", "conclusion": "SUCCESS",
         "detailsUrl": "https://github.com/o/r/actions/runs/36038030899/job/107762908591"},
        {"__typename": "CheckRun", "name": "docs", "status": "COMPLETED", "conclusion": "FAILURE",
         "detailsUrl": "https://github.com/o/r/actions/runs/36038262943/job/107763684465"},
        {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
        {"__typename": "CheckRun", "name": "optional", "status": "COMPLETED", "conclusion": "SKIPPED"},
        {"__typename": "StatusContext", "context": "CodeRabbit", "state": "SUCCESS", "targetUrl": "https://coderabbit.ai"}
      ]
    }"#;

    #[test]
    fn parses_the_pr_its_files_and_both_kinds_of_check() {
        let pr = parse_pr(PR).unwrap();
        assert_eq!(pr.number, 579);
        assert_eq!(pr.author, "vitali87");
        assert_eq!(
            (pr.head_ref.as_str(), pr.base_ref.as_str()),
            ("feat/x", "main")
        );
        assert_eq!(pr.files.len(), 2);
        assert_eq!(
            pr.files[0],
            PrFile {
                path: "src/sarif/view.rs".into(),
                additions: 1048,
                deletions: 0,
                change: "ADDED".into()
            }
        );
        let states: Vec<(&str, CheckState)> = pr
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("clippy + tests", CheckState::Pass),
                ("docs", CheckState::Fail),
                ("build", CheckState::Pending),
                ("optional", CheckState::Neutral),
                ("CodeRabbit", CheckState::Pass),
            ]
        );
        assert_eq!(pr.checks[4].url.as_deref(), Some("https://coderabbit.ai"));
    }

    #[test]
    fn the_fetch_asks_gh_for_every_field_the_parser_reads() {
        let args = view_args("579");
        assert_eq!(&args[..3], ["pr", "view", "579"]);
        assert_eq!(args[3], "--json");
        for field in ["files", "statusCheckRollup", "headRefOid", "author", "url"] {
            assert!(args[4].split(',').any(|f| f == field), "{field} requested");
        }
    }

    #[test]
    fn a_whole_pr_patch_splits_per_file() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/old.txt b/old.txt\ndeleted file mode 100644\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n";
        let files = split_diff_by_file(patch);
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            vec!["old.txt", "src/a.rs"]
        );
        assert!(files["src/a.rs"].starts_with("diff --git a/src/a.rs"));
        assert!(files["src/a.rs"].contains("+y"));
        assert!(!files["src/a.rs"].contains("gone"), "sections do not bleed");
        assert!(files["old.txt"].contains("-gone"));
    }

    #[test]
    fn failing_check_logs_come_from_gh_run_view() {
        assert_eq!(
            log_args(7, Some(8)),
            vec!["run", "view", "7", "--job", "8", "--log-failed"]
        );
        assert_eq!(log_args(7, None), vec!["run", "view", "7", "--log-failed"]);
    }

    #[test]
    fn pr_selectors_parse_as_typed_or_pasted() {
        let sel = |s: &str| parse_pr_selector(s);
        assert_eq!(sel("579").as_deref(), Some("579"));
        assert_eq!(sel(" #579 ").as_deref(), Some("579"));
        // A URL keeps its repository, so another repository's PR opens.
        assert_eq!(
            sel("https://github.com/x/y/pull/579").as_deref(),
            Some("https://github.com/x/y/pull/579")
        );
        assert_eq!(
            sel("https://github.com/o/r/pull/579/files").as_deref(),
            Some("https://github.com/o/r/pull/579")
        );
        assert_eq!(sel("https://github.com/o/r/pull/x"), None);
        assert_eq!(sel("abc"), None);
        assert_eq!(sel(""), None);
        assert_eq!(sel("#0"), None, "no PR zero");
    }

    #[test]
    fn open_prs_parse_for_the_picker() {
        let items = parse_pr_list(
            r#"[{"number": 7, "title": "Fix it", "author": {"login": "ada"}, "url": "https://github.com/o/r/pull/7", "isDraft": true},
                {"title": "no number"}]"#,
        )
        .unwrap();
        assert_eq!(
            items,
            vec![PrListItem {
                number: 7,
                title: "Fix it".into(),
                author: "ada".into(),
                url: "https://github.com/o/r/pull/7".into(),
                draft: true,
            }]
        );
        assert!(parse_pr_list("{}").is_err());
        assert!(list_args().contains(&String::from("number,title,author,url,isDraft")));
    }

    #[test]
    fn the_startup_pr_is_taken_once() {
        set_startup_pr(String::from("579"));
        assert_eq!(take_startup_pr().as_deref(), Some("579"));
        assert_eq!(take_startup_pr(), None);
    }

    #[test]
    fn gh_runs_bounded_and_reports_stdout_or_stderr() {
        let dir = std::env::temp_dir();
        let sh = |script: &str| vec![String::from("-c"), String::from(script)];
        let t = std::time::Duration::from_secs(5);
        assert_eq!(
            run_gh("/bin/sh", &sh("echo out"), &dir, t).as_deref(),
            Ok("out\n")
        );
        assert_eq!(
            run_gh("/bin/sh", &sh("echo why >&2; exit 1"), &dir, t),
            Err(String::from("why"))
        );
        let started = std::time::Instant::now();
        let slow = run_gh(
            "/bin/sh",
            &sh("sleep 5"),
            &dir,
            std::time::Duration::from_millis(200),
        );
        assert!(slow.unwrap_err().contains("took longer"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "killed at the bound"
        );
        assert!(
            run_gh("/nonexistent/gh", &[], &dir, t)
                .unwrap_err()
                .contains("could not run")
        );
    }

    #[test]
    fn the_viewed_key_names_the_repository_and_the_pr() {
        let pr = parse_pr(PR).unwrap();
        assert_eq!(review_key(&pr), "o/r#579");
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_pr("not json").is_err());
        assert!(parse_pr("{}").is_err(), "a PR needs at least a number");
    }

    #[test]
    fn run_and_job_ids_come_from_the_details_url() {
        assert_eq!(
            run_and_job("https://github.com/o/r/actions/runs/36038262943/job/107763684465"),
            Some((36038262943, Some(107763684465)))
        );
        assert_eq!(
            run_and_job("https://github.com/o/r/actions/runs/5"),
            Some((5, None))
        );
        assert_eq!(run_and_job("https://coderabbit.ai"), None);
    }

    #[test]
    fn summary_counts_viewed_files_and_check_states() {
        let pr = parse_pr(PR).unwrap();
        let mut viewed = BTreeSet::new();
        viewed.insert("Cargo.toml".to_string());
        assert_eq!(
            summary(&pr, &viewed),
            "1 of 2 viewed · 2 passed · 1 failed · 1 pending"
        );
    }

    #[test]
    fn viewed_marks_persist_per_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewed.json");
        let mut store = ViewedStore::load(&path);
        assert!(store.toggle("o/r#579", "a.rs"), "first toggle marks viewed");
        store.save(&path).unwrap();
        let back = ViewedStore::load(&path);
        assert!(back.viewed("o/r#579").contains("a.rs"));
        assert!(back.viewed("o/r#580").is_empty(), "other PRs are separate");
        let mut back = back;
        assert!(!back.toggle("o/r#579", "a.rs"), "second toggle clears it");
        assert!(!back.viewed("o/r#579").contains("a.rs"));
    }

    #[test]
    fn setting_a_mark_outright_adds_or_clears_it() {
        let mut store = ViewedStore::default();
        store.set("o/r#1", "a.rs", true);
        store.set("o/r#1", "a.rs", true);
        assert!(store.viewed("o/r#1").contains("a.rs"));
        store.set("o/r#1", "a.rs", false);
        assert_eq!(store, ViewedStore::default(), "an emptied PR is dropped");
        store.set("o/r#1", "b.rs", false);
        assert_eq!(store, ViewedStore::default());
    }

    // ── GitHub's viewed state (#365) ────────────────────────────────────

    #[test]
    fn the_viewed_query_names_the_repository_the_pr_and_the_page() {
        let args = viewed_query_args("o/r", 579, None).unwrap();
        assert_eq!(&args[..3], ["api", "graphql", "-f"]);
        assert!(args[3].starts_with("query=query("), "{}", args[3]);
        assert!(args[3].contains("viewerViewedState") && args[3].contains("endCursor"));
        assert!(!args[3].contains('\n'), "one line per call in a log");
        assert_eq!(
            &args[4..],
            ["-f", "owner=o", "-f", "name=r", "-F", "number=579"]
        );
        let next = viewed_query_args("o/r", 579, Some("Y3Vy")).unwrap();
        assert_eq!(&next[next.len() - 2..], ["-f", "after=Y3Vy"]);
        assert_eq!(viewed_query_args("", 1, None), None, "not on GitHub");
        assert_eq!(viewed_query_args("o/", 1, None), None);
        assert_eq!(viewed_query_args("a/b/c", 1, None), None);
    }

    #[test]
    fn a_viewed_page_parses_to_states_and_the_next_cursor() {
        let page = parse_viewed_page(
            r#"{"data":{"repository":{"pullRequest":{"id":"PR_1","files":{
                "nodes":[{"path":"a.rs","viewerViewedState":"VIEWED"},
                         {"path":"b.rs","viewerViewedState":"DISMISSED"},
                         {"path":"c.rs","viewerViewedState":"UNVIEWED"}],
                "pageInfo":{"hasNextPage":true,"endCursor":"Y3Vy"}}}}}}"#,
        )
        .unwrap();
        assert_eq!(page.pr_id, "PR_1");
        assert_eq!(page.states["a.rs"], ViewedState::Viewed);
        assert_eq!(page.states["b.rs"], ViewedState::Dismissed);
        assert_eq!(page.states["c.rs"], ViewedState::Unviewed);
        assert!(!ViewedState::Dismissed.is_viewed(), "changed since viewed");
        assert_eq!(page.next.as_deref(), Some("Y3Vy"));
        let last = parse_viewed_page(
            r#"{"data":{"repository":{"pullRequest":{"id":"PR_1","files":{"nodes":[],
                "pageInfo":{"hasNextPage":false,"endCursor":"Zm9v"}}}}}}"#,
        )
        .unwrap();
        assert_eq!(last.next, None, "no next page past the last");
    }

    #[test]
    fn a_refused_or_empty_viewed_answer_is_an_error() {
        let refused = parse_viewed_page(
            r#"{"data":{"repository":null},"errors":[{"message":"Could not resolve to a Repository"}]}"#,
        );
        assert!(refused.unwrap_err().contains("Could not resolve"));
        assert!(parse_viewed_page(r#"{"data":{"repository":{"pullRequest":null}}}"#).is_err());
        assert!(parse_viewed_page("{}").is_err());
        assert!(parse_viewed_page("").is_err());
    }

    #[test]
    fn marking_viewed_sends_the_matching_mutation() {
        let mark = mark_viewed_args("PR_1", "src/a b.rs", true);
        assert_eq!(&mark[..3], ["api", "graphql", "-f"]);
        assert!(mark[3].contains("markFileAsViewed(input: {pullRequestId: $id, path: $path})"));
        assert!(!mark[3].contains("unmark"));
        assert_eq!(&mark[4..], ["-f", "id=PR_1", "-f", "path=src/a b.rs"]);
        let unmark = mark_viewed_args("PR_1", "a.rs", false);
        assert!(unmark[3].contains("unmarkFileAsViewed("), "{}", unmark[3]);
    }

    // ── review payloads (#366, #368) ────────────────────────────────────

    const PATCH: &str = "@@ -10,4 +10,5 @@ fn a() {\n ctx one\n-old\n+new one\n+new two\n ctx two\n@@ -40,2 +41,2 @@\n ctx three\n-gone\n+here\n";

    #[test]
    fn commentable_lines_are_the_right_side_of_every_hunk() {
        let lines = commentable_lines(PATCH);
        // Hunk 1 starts at 10: ctx one (10), new one (11), new two (12),
        // ctx two (13). Hunk 2 starts at 41: ctx three (41), here (42).
        assert_eq!(
            lines.into_iter().collect::<Vec<_>>(),
            vec![10, 11, 12, 13, 41, 42]
        );
        assert!(commentable_lines("").is_empty());
        assert!(commentable_lines("Binary files differ").is_empty());
    }

    #[test]
    fn a_review_is_one_request_with_every_pending_comment() {
        let pending = vec![
            PendingComment {
                path: "src/a.rs".into(),
                line: 11,
                body: "why?".into(),
            },
            PendingComment {
                path: "src/b.rs".into(),
                line: 3,
                body: "rename".into(),
            },
        ];
        let v = review_payload(
            "abc123",
            ReviewEvent::RequestChanges,
            "needs work",
            &pending,
        );
        assert_eq!(v["commit_id"], "abc123");
        assert_eq!(v["event"], "REQUEST_CHANGES");
        assert_eq!(v["body"], "needs work");
        assert_eq!(v["comments"].as_array().unwrap().len(), 2);
        assert_eq!(v["comments"][0]["path"], "src/a.rs");
        assert_eq!(v["comments"][0]["line"], 11);
        assert_eq!(v["comments"][0]["side"], "RIGHT");
        assert_eq!(v["comments"][1]["body"], "rename");
        assert_eq!(ReviewEvent::Approve.api(), "APPROVE");
        assert_eq!(ReviewEvent::Comment.api(), "COMMENT");
    }

    #[test]
    fn export_splits_boxes_into_inline_comments_and_a_summary() {
        let mut diff: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        diff.insert("src/a.rs".into(), commentable_lines(PATCH));
        let boxes = vec![
            ExportBox {
                path: "src/a.rs".into(),
                line: 11,
                body: "one".into(),
                ai: false,
            },
            ExportBox {
                path: "src/a.rs".into(),
                line: 12,
                body: "two".into(),
                ai: true,
            },
            ExportBox {
                path: "src/a.rs".into(),
                line: 42,
                body: "three".into(),
                ai: false,
            },
            ExportBox {
                path: "src/a.rs".into(),
                line: 99,
                body: "off diff".into(),
                ai: false,
            },
        ];
        let (inline, summary) = split_export(&boxes, &diff);
        assert_eq!(inline.len(), 3);
        assert_eq!(
            inline[1].body, "(AI-authored) two",
            "navigator boxes say so"
        );
        assert_eq!(inline[0].body, "one");
        assert!(summary.contains("src/a.rs:99"), "{summary}");
        assert!(summary.contains("off diff"), "{summary}");
        let (_, none) = split_export(&boxes[..1], &diff);
        assert!(none.is_empty(), "no summary when everything is inline");
    }

    #[test]
    fn a_corrupt_store_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewed.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(ViewedStore::load(&path), ViewedStore::default());
    }
}
