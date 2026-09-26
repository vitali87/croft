//! The write side of PR review in the editor (#366): reply to a thread,
//! resolve or unresolve it, and submit a review with the comments written
//! in croft. Each is a `gh` call run off the frame loop; [`run`] does the
//! work and returns what the app should show.

use crate::review_threads::{PendingComment, ReviewEvent};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a posted export settles once it lands: navigator notes to remove
/// and sticky notes to mark resolved. It rides the job and comes back in
/// its outcome, so only the submission that carried them can settle them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settles {
    pub notes: Vec<u64>,
    pub sticky: Vec<String>,
    /// The pending comments this submission posted: only these leave the
    /// pending list, so one written while it ran is kept.
    pub pending: Vec<PendingComment>,
}

#[derive(Clone, Debug)]
pub enum Job {
    Reply {
        number: String,
        thread: u64,
        text: String,
    },
    Resolve {
        thread: u64,
        node: String,
        resolve: bool,
    },
    Submit {
        number: String,
        event: ReviewEvent,
        summary: String,
        pending: Vec<PendingComment>,
        settles: Settles,
    },
    /// Find the branch's PR and read `rel`'s review threads with their
    /// resolution (#366). `path` rides along so the threads land on the file
    /// they were read for.
    LoadThreads { path: PathBuf, rel: String },
    /// Find the branch's PR and count which of `comments` GitHub will take
    /// inline, for the export preview (#368).
    ExportPreview {
        comments: Vec<PendingComment>,
        notes: Vec<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Replied {
        thread: u64,
        text: String,
    },
    Resolved {
        thread: u64,
        resolved: bool,
    },
    Submitted {
        inline: usize,
        folded: usize,
        settles: Settles,
    },
    Threads {
        root: PathBuf,
        number: String,
        path: PathBuf,
        rel: String,
        threads: Vec<crate::review_threads::Thread>,
        /// Thread id to (GraphQL node id, resolved).
        states: std::collections::HashMap<u64, (String, bool)>,
    },
    ExportReady {
        root: PathBuf,
        number: String,
        comments: Vec<PendingComment>,
        notes: Vec<u64>,
        inline: usize,
    },
    /// No PR for the branch; the status says so.
    NoPr,
    /// A submission failed: the next one may go.
    SubmitFailed(String),
    /// Any other job failed; nothing was in flight on its account.
    Failed(String),
}

/// The PR number for the branch checked out in `root`.
fn pr_number(program: &str, root: &Path) -> Option<String> {
    gh(
        program,
        root,
        &["pr", "view", "--json", "number", "--jq", ".number"],
        None,
    )
    .ok()
    .map(|n| n.trim().to_string())
    .filter(|n| !n.is_empty())
}

/// gh's first stderr line, which names the problem (scope, rate limit, a
/// line outside the diff) better than anything croft could say.
fn gh_error(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    err.trim().lines().next().unwrap_or("gh failed").to_string()
}

fn gh(program: &str, root: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| format!("could not run gh: {e}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(input.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("gh failed: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(gh_error(&out))
    }
}

/// Carry out `job` with `program` (normally `gh`) in `root`.
pub fn run(program: &str, root: &Path, job: Job) -> Outcome {
    match job {
        Job::Reply {
            number,
            thread,
            text,
        } => {
            let endpoint =
                format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments/{thread}/replies");
            let body = format!("body={text}");
            match gh(
                program,
                root,
                &["api", "-X", "POST", &endpoint, "-f", &body],
                None,
            ) {
                Ok(_) => Outcome::Replied { thread, text },
                Err(e) => Outcome::Failed(format!("Reply failed: {e}")),
            }
        }
        Job::Resolve {
            thread,
            node,
            resolve,
        } => {
            let query = format!("query={}", crate::review_threads::resolve_mutation(resolve));
            let id = format!("id={node}");
            match gh(
                program,
                root,
                &["api", "graphql", "-f", &query, "-f", &id],
                None,
            ) {
                Ok(_) => Outcome::Resolved {
                    thread,
                    resolved: resolve,
                },
                Err(e) => Outcome::Failed(format!("Could not change the thread: {e}")),
            }
        }
        Job::Submit {
            number,
            event,
            summary,
            pending,
            settles,
        } => {
            // The diff decides which comments GitHub will take inline; a
            // failure to read it folds every comment into the summary rather
            // than risking a refused review.
            let diff = gh(program, root, &["pr", "diff", &number], None).unwrap_or_default();
            let mut lines: std::collections::HashMap<String, std::collections::HashSet<usize>> =
                std::collections::HashMap::new();
            for c in &pending {
                lines
                    .entry(c.path.clone())
                    .or_insert_with(|| crate::review_threads::commentable_lines(&diff, &c.path));
            }
            let commentable =
                |path: &str, line: usize| lines.get(path).is_some_and(|s| s.contains(&line));
            let payload =
                crate::review_threads::review_payload(event, &summary, &pending, &commentable);
            let inline = payload["comments"].as_array().map_or(0, Vec::len);
            let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
            match gh(
                program,
                root,
                &["api", "-X", "POST", &endpoint, "--input", "-"],
                Some(&payload.to_string()),
            ) {
                Ok(_) => Outcome::Submitted {
                    inline,
                    folded: pending.len() - inline,
                    settles,
                },
                Err(e) => Outcome::SubmitFailed(format!("Review not submitted: {e}")),
            }
        }
        Job::LoadThreads { path, rel } => {
            let Some(number) = pr_number(program, root) else {
                return Outcome::NoPr;
            };
            let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments");
            let json = match gh(program, root, &["api", &endpoint, "--paginate"], None) {
                Ok(j) => j,
                Err(e) => return Outcome::Failed(format!("Could not read the PR's comments: {e}")),
            };
            let threads: Vec<_> = crate::review_threads::parse_threads(&json)
                .into_iter()
                .filter(|t| t.path == rel)
                .collect();
            // Resolution and the node id Resolve needs live only in GraphQL.
            // Best effort: without it every thread reads as unresolved, the
            // safe default. Every page: past 100 threads the rest would
            // otherwise lack the id Resolve needs.
            let mut states = std::collections::HashMap::new();
            let mut after: Option<String> = None;
            loop {
                let mut args = vec![
                    String::from("api"),
                    String::from("graphql"),
                    String::from("-F"),
                    String::from("owner={owner}"),
                    String::from("-F"),
                    String::from("repo={repo}"),
                    String::from("-F"),
                    format!("number={number}"),
                    String::from("-f"),
                    format!("query={}", crate::review_threads::THREADS_QUERY),
                ];
                if let Some(cursor) = &after {
                    args.push(String::from("-f"));
                    args.push(format!("after={cursor}"));
                }
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let Ok(page) = gh(program, root, &args, None) else {
                    break;
                };
                states.extend(crate::review_threads::parse_thread_states(&page));
                match crate::review_threads::next_page(&page) {
                    Some(cursor) if after.as_deref() != Some(cursor.as_str()) => {
                        after = Some(cursor);
                    }
                    _ => break,
                }
            }
            Outcome::Threads {
                root: root.to_path_buf(),
                number,
                path,
                rel,
                threads,
                states,
            }
        }
        Job::ExportPreview { comments, notes } => {
            let Some(number) = pr_number(program, root) else {
                return Outcome::NoPr;
            };
            // Which comments GitHub will take inline, so one folded into the
            // summary is no surprise after posting.
            let diff = gh(program, root, &["pr", "diff", &number], None).unwrap_or_default();
            let inline = comments
                .iter()
                .filter(|c| {
                    crate::review_threads::commentable_lines(&diff, &c.path).contains(&(c.line + 1))
                })
                .count();
            Outcome::ExportReady {
                root: root.to_path_buf(),
                number,
                comments,
                notes,
                inline,
            }
        }
    }
}

/// Run `job` on a thread; the outcome arrives on `tx`.
pub fn spawn(program: String, root: PathBuf, job: Job, tx: std::sync::mpsc::Sender<Outcome>) {
    std::thread::spawn(move || {
        let _ = tx.send(run(&program, &root, job));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `gh` that logs its arguments and stdin, answers `pr diff`
    /// with a one-hunk diff, and fails when told to.
    fn fake_gh(dir: &Path, fail: bool) -> String {
        let path = dir.join("gh");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\nif [ \"$1\" = pr ]; then printf 'diff --git a/a.rs b/a.rs\\n--- a/a.rs\\n+++ b/a.rs\\n@@ -1,1 +1,2 @@\\n x\\n+y\\n'; exit 0; fi\n{fail}cat > '{stdin}' 2>/dev/null\necho '{{}}'\n",
            log = dir.join("log").display(),
            stdin = dir.join("stdin").display(),
            fail = if fail {
                "echo 'HTTP 422: Unprocessable' >&2; exit 1\n"
            } else {
                ""
            },
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn a_reply_posts_to_the_threads_replies_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = fake_gh(tmp.path(), false);
        let out = run(
            &gh,
            tmp.path(),
            Job::Reply {
                number: "7".into(),
                thread: 42,
                text: "fixed".into(),
            },
        );
        assert_eq!(
            out,
            Outcome::Replied {
                thread: 42,
                text: "fixed".into()
            }
        );
        let log = std::fs::read_to_string(tmp.path().join("log")).unwrap();
        assert!(
            log.contains(
                "api -X POST repos/{owner}/{repo}/pulls/7/comments/42/replies -f body=fixed"
            ),
            "{log}"
        );
    }

    #[test]
    fn a_submitted_review_sends_inline_and_folded_comments_and_the_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = fake_gh(tmp.path(), false);
        let out = run(
            &gh,
            tmp.path(),
            Job::Submit {
                number: "7".into(),
                event: ReviewEvent::Approve,
                summary: "LGTM".into(),
                pending: vec![
                    PendingComment {
                        path: "a.rs".into(),
                        line: 1,
                        body: "nice".into(),
                    },
                    PendingComment {
                        path: "a.rs".into(),
                        line: 30,
                        body: "later".into(),
                    },
                ],
                settles: Settles::default(),
            },
        );
        assert_eq!(
            out,
            Outcome::Submitted {
                inline: 1,
                folded: 1,
                settles: Settles::default(),
            }
        );
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("stdin")).unwrap())
                .unwrap();
        assert_eq!(sent["event"], "APPROVE");
        assert_eq!(sent["comments"][0]["line"], 2);
        assert!(sent["body"].as_str().unwrap().contains("a.rs:31"));
    }

    #[test]
    fn a_refusal_reports_ghs_own_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = fake_gh(tmp.path(), true);
        let out = run(
            &gh,
            tmp.path(),
            Job::Resolve {
                thread: 1,
                node: "PRRT_x".into(),
                resolve: true,
            },
        );
        assert_eq!(
            out,
            Outcome::Failed("Could not change the thread: HTTP 422: Unprocessable".into())
        );
    }
}
