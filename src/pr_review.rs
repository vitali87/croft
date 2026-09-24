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

/// The `gh` arguments that fetch pull request `number` in the shape
/// [`parse_pr`] reads.
pub fn view_args(number: u64) -> Vec<String> {
    vec![
        String::from("pr"),
        String::from("view"),
        number.to_string(),
        String::from("--json"),
        String::from(PR_FIELDS),
    ]
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
        let args = view_args(579);
        assert_eq!(&args[..3], ["pr", "view", "579"]);
        assert_eq!(args[3], "--json");
        for field in ["files", "statusCheckRollup", "headRefOid", "author", "url"] {
            assert!(args[4].split(',').any(|f| f == field), "{field} requested");
        }
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
