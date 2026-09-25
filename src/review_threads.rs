//! Turning GitHub review threads into the editor's comment boxes (#366).
//!
//! The navigator's comment boxes — unnumbered blocks between the lines they
//! belong to, with a reply field and `F4` to hop — are already the UI a code
//! review needs. This maps GitHub's review-comment JSON onto them.
//!
//! # Why `line` can be null, and why that is the whole problem
//!
//! GitHub anchors a review comment to a line in the diff at the time it was
//! written. When the branch moves under it, the comment becomes OUTDATED and
//! the API reports `line: null` while keeping `original_line`. Both fields
//! matter and they mean different things:
//!
//! * `line` — where the comment is NOW. Trustworthy.
//! * `original_line` — where it was when written. A guess about today.
//!
//! Rendering an outdated comment at `original_line` silently attaches
//! someone's objection to whatever code now occupies that number, which may
//! be a different function entirely. So an outdated thread is anchored but
//! MARKED, never anchored silently — the reviewer needs to know the line
//! under it is not the line the comment was about.
//!
//! A thread with neither line is not placeable at all. It is reported as
//! file-level rather than dropped: a comment nobody can see is worse than
//! one shown at the top of the file with a note saying where it belongs.

/// Where a review thread belongs in the buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The comment's line is current: hang the box under this 0-based line.
    At(usize),
    /// The branch moved under it. The line is a best guess from
    /// `original_line`, and the box must say so.
    Outdated(usize),
    /// No line at all — a review comment on the file rather than a line.
    FileLevel,
}

/// One review thread, as croft needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub path: String,
    pub anchor: Anchor,
    /// Whether the thread is resolved.
    ///
    /// NOT available from the REST comments endpoint — verified against the
    /// live API, where no field matching `resolv` exists on a review
    /// comment. Resolution lives only on GraphQL's `reviewThreads.isResolved`,
    /// which is a different query keyed on threads rather than comments.
    ///
    /// So this is populated by the CALLER when it has that data, and is
    /// `false` otherwise. It defaults to unresolved rather than resolved
    /// because the wrong default here hides live objections behind a dimmed
    /// box — the one outcome a review tool must not produce.
    pub resolved: bool,
}

/// Parse `gh api repos/O/R/pulls/N/comments` output into threads.
///
/// Skips entries croft cannot place a box for at all — one with no `path`
/// is not about a file. Everything else is kept, because a review comment
/// the reviewer never sees is the failure this feature exists to prevent.
pub fn parse_threads(json: &str) -> Vec<Thread> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    // A reply is its own comment object carrying `in_reply_to_id`; it joins
    // its root's box as a further paragraph, so a conversation reads as one
    // thread and a reply posts to the root it belongs to.
    let mut roots: Vec<Thread> = Vec::new();
    let mut replies: Vec<(u64, String, String)> = Vec::new();
    for v in items {
        match v.get("in_reply_to_id").and_then(serde_json::Value::as_u64) {
            Some(root) => {
                if let Some(t) = thread_from(v) {
                    replies.push((root, t.author, t.body));
                }
            }
            None => roots.extend(thread_from(v)),
        }
    }
    for (root, author, body) in replies {
        if let Some(t) = roots.iter_mut().find(|t| t.id == root) {
            t.body.push_str(&format!("\n\n{author}: {body}"));
        }
    }
    roots
}

/// Resolution state and GraphQL node id per thread, keyed by the thread's
/// first comment's REST id, from [`THREADS_QUERY`]'s output.
pub fn parse_thread_states(json: &str) -> std::collections::HashMap<u64, (String, bool)> {
    let mut out = std::collections::HashMap::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let nodes = v
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(serde_json::Value::as_array);
    for n in nodes.into_iter().flatten() {
        let id = n.get("id").and_then(serde_json::Value::as_str);
        let resolved = n.get("isResolved").and_then(serde_json::Value::as_bool);
        let first = n
            .pointer("/comments/nodes/0/databaseId")
            .and_then(serde_json::Value::as_u64);
        if let (Some(id), Some(resolved), Some(first)) = (id, resolved, first) {
            out.insert(first, (id.to_string(), resolved));
        }
    }
    out
}

/// GraphQL for each review thread's node id, resolution and first comment.
/// Takes `$owner`, `$repo` and `$number`.
pub const THREADS_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved comments(first:1){nodes{databaseId}}}}}}}";

/// The GraphQL mutation that resolves (or unresolves) thread `$id`.
pub fn resolve_mutation(resolve: bool) -> &'static str {
    if resolve {
        "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}"
    } else {
        "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{isResolved}}}"
    }
}

/// What an exported navigator comment starts with unless
/// `review_ai_prefix` says otherwise, so reviewers know which comments a
/// model wrote.
pub const DEFAULT_AI_PREFIX: &str = "[AI, croft navigator] ";

/// A comment written in croft and not yet submitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingComment {
    /// Repo-relative path, forward slashes.
    pub path: String,
    /// 0-based buffer line.
    pub line: usize,
    pub body: String,
}

/// The verdict a review is submitted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// The new-file line numbers (1-based) a review may comment on in `path`,
/// read from the PR's unified diff: the added and context lines of its
/// hunks. GitHub refuses a whole review if one comment falls outside them.
pub fn commentable_lines(diff: &str, path: &str) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    let mut in_file = false;
    let mut line = 0usize;
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("+++ ") {
            in_file = rest.strip_prefix("b/").unwrap_or(rest) == path;
            continue;
        }
        if l.starts_with("diff --git ") {
            in_file = false;
            continue;
        }
        if !in_file {
            continue;
        }
        if let Some(h) = l.strip_prefix("@@ ") {
            // `@@ -a,b +c,d @@`: new-side hunk starts at c.
            line = h
                .split_whitespace()
                .find_map(|t| t.strip_prefix('+'))
                .and_then(|t| t.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            continue;
        }
        if line == 0 {
            continue;
        }
        match l.chars().next() {
            Some('+') | Some(' ') => {
                out.insert(line);
                line += 1;
            }
            Some('-') | Some('\\') => {}
            _ => {
                out.insert(line);
                line += 1;
            }
        }
    }
    out
}

/// The body of `POST /pulls/N/reviews`. Pending comments on lines the diff
/// covers go inline; the rest are listed in the summary with their place,
/// so nothing written is lost and GitHub doesn't refuse the review.
pub fn review_payload(
    event: ReviewEvent,
    summary: &str,
    pending: &[PendingComment],
    commentable: &dyn Fn(&str, usize) -> bool,
) -> serde_json::Value {
    let (inline, off): (Vec<&PendingComment>, Vec<&PendingComment>) = pending
        .iter()
        .partition(|c| commentable(&c.path, c.line + 1));
    let mut body = summary.trim().to_string();
    if !off.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("Comments on lines outside the diff:\n");
        for c in &off {
            body.push_str(&format!("\n- `{}:{}`: {}", c.path, c.line + 1, c.body));
        }
    }
    serde_json::json!({
        "event": event.api_name(),
        "body": body,
        "comments": inline.iter().map(|c| serde_json::json!({
            "path": c.path,
            "line": c.line + 1,
            "side": "RIGHT",
            "body": c.body,
        })).collect::<Vec<_>>(),
    })
}

/// One comment object to a [`Thread`].
fn thread_from(v: &serde_json::Value) -> Option<Thread> {
    let path = v.get("path")?.as_str()?.to_string();
    let id = v.get("id").and_then(serde_json::Value::as_u64)?;
    let author = v
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("someone")
        .to_string();
    let body = v
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Thread {
        id,
        author,
        body,
        path,
        anchor: anchor_of(v),
        // The REST endpoint carries no resolution state at all, so this is
        // read opportunistically: a caller that has merged in GraphQL's
        // `isResolved` can put it on the object, and one that has not gets
        // `false`. Unresolved is the safe default — the opposite hides a
        // live objection behind a dimmed box.
        resolved: v
            .get("resolved")
            .or_else(|| v.get("isResolved"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Where a comment object anchors.
///
/// `line` is 1-based in the API and 0-based in the buffer, so every path
/// converts once here rather than at each call site — two conversions is
/// how an off-by-one reaches the screen.
fn anchor_of(v: &serde_json::Value) -> Anchor {
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
    // A CURRENT line wins outright. GitHub sets it to null once the branch
    // has moved under the comment.
    if let Some(line) = num("line").filter(|n| *n > 0) {
        return Anchor::At((line - 1) as usize);
    }
    // Outdated: place it where it WAS, and say so. Silently using this as
    // if it were current attaches someone's objection to whatever code now
    // occupies that number.
    if let Some(line) = num("original_line").filter(|n| *n > 0) {
        return Anchor::Outdated((line - 1) as usize);
    }
    Anchor::FileLevel
}

impl Thread {
    /// The box's title row: who, and whether it can be trusted to be here.
    ///
    /// Takes the buffer's length because a clamped thread has to say where
    /// it really was.
    ///
    /// Two outdated threads at lines 5000 and 6000 of a 100-line file both
    /// clamp onto the last line, and stacked boxes give no hint that their
    /// anchors were 900 lines apart. Naming the original line is the only
    /// thing that distinguishes them.
    pub fn title_for(&self, buffer_lines: usize) -> String {
        let mut t = self.author.clone();
        if self.resolved {
            t.push_str(" \u{b7} resolved");
        }
        if matches!(self.anchor, Anchor::Outdated(_)) {
            // Named in the title rather than only in a colour, because a
            // reviewer skimming boxes reads titles and a colour is exactly
            // what a screenshot or a colour-blind reader loses.
            t.push_str(" \u{b7} outdated");
        }
        // Clamped: say where it was, or two threads from far apart stack on
        // the last line indistinguishably. Applies to a CURRENT anchor too,
        // not just an outdated one: the buffer is whatever the user has
        // edited it to since loading, so any line can end up past the end.
        if let Anchor::At(line) | Anchor::Outdated(line) = self.anchor
            && line >= buffer_lines
        {
            t.push_str(&format!(" \u{b7} was line {}", line + 1));
        }
        if matches!(self.anchor, Anchor::FileLevel) {
            t.push_str(" \u{b7} on the file");
        }
        t
    }

    /// The 0-based line to hang the box under, clamped to the buffer.
    ///
    /// Clamped because an outdated line can point past the end of a file
    /// that has since shrunk, and a box at line 4000 of a 100-line file is
    /// invisible — which is the same as losing the comment.
    pub fn box_line(&self, buffer_lines: usize) -> usize {
        let last = buffer_lines.saturating_sub(1);
        match self.anchor {
            Anchor::At(l) | Anchor::Outdated(l) => l.min(last),
            Anchor::FileLevel => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CURRENT line anchors trustworthily; an OUTDATED one is anchored but
    /// marked.
    ///
    /// This is the whole feature's honesty. GitHub nulls `line` once the
    /// branch moves under a comment while keeping `original_line`, so using
    /// the second as if it were the first silently attaches someone's
    /// objection to whatever code now occupies that number — which after a
    /// rebase is routinely a different function.
    #[test]
    fn an_outdated_thread_is_marked_rather_than_placed_silently() {
        let json = r#"[
          {"id":1,"path":"src/a.rs","line":42,"original_line":10,
           "user":{"login":"ada"},"body":"this leaks"},
          {"id":2,"path":"src/a.rs","line":null,"original_line":7,
           "user":{"login":"bob"},"body":"stale point"}
        ]"#;
        let threads = parse_threads(json);
        assert_eq!(threads.len(), 2);

        // Current: `line` wins over `original_line`, and converts to 0-based.
        assert_eq!(threads[0].anchor, Anchor::At(41));
        assert!(
            !threads[0].title_for(usize::MAX).contains("outdated"),
            "a current thread must not be labelled outdated: {}",
            threads[0].title_for(usize::MAX)
        );

        // Outdated: placed from `original_line`, and SAID so.
        assert_eq!(threads[1].anchor, Anchor::Outdated(6));
        assert!(
            threads[1].title_for(usize::MAX).contains("outdated"),
            "an outdated thread must say so in its title: {}",
            threads[1].title_for(usize::MAX)
        );
        assert!(
            threads[1].title_for(usize::MAX).contains("bob"),
            "and name its author"
        );
    }

    #[test]
    fn replies_join_their_root_thread() {
        let json = r#"[
          {"id":1,"path":"a.rs","line":3,"user":{"login":"ada"},"body":"why?"},
          {"id":2,"path":"a.rs","line":3,"in_reply_to_id":1,"user":{"login":"bob"},"body":"because"},
          {"id":3,"path":"a.rs","line":9,"user":{"login":"cy"},"body":"typo"}
        ]"#;
        let t = parse_threads(json);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].body, "why?\n\nbob: because");
    }

    #[test]
    fn thread_states_come_from_graphql_keyed_by_first_comment() {
        let json = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
          {"id":"PRRT_a","isResolved":true,"comments":{"nodes":[{"databaseId":11}]}},
          {"id":"PRRT_b","isResolved":false,"comments":{"nodes":[{"databaseId":12}]}}
        ]}}}}}"#;
        let s = parse_thread_states(json);
        assert_eq!(s.get(&11), Some(&(String::from("PRRT_a"), true)));
        assert_eq!(s.get(&12).map(|x| x.1), Some(false));
    }

    #[test]
    fn commentable_lines_are_the_new_side_of_the_files_hunks() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n@@ -20,2 +21,2 @@ fn f()\n x\n+y\ndiff --git a/z.rs b/z.rs\n--- a/z.rs\n+++ b/z.rs\n@@ -1 +1 @@\n+q\n";
        let mut got: Vec<usize> = commentable_lines(diff, "x.rs").into_iter().collect();
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4, 21, 22]);
        assert_eq!(commentable_lines(diff, "z.rs").len(), 1);
        assert!(commentable_lines(diff, "nope.rs").is_empty());
    }

    #[test]
    fn off_diff_comments_move_into_the_summary() {
        let pending = vec![
            PendingComment {
                path: "a.rs".into(),
                line: 4,
                body: "inline".into(),
            },
            PendingComment {
                path: "a.rs".into(),
                line: 99,
                body: "far away".into(),
            },
        ];
        let v = review_payload(
            ReviewEvent::RequestChanges,
            "Looks close.",
            &pending,
            &|_, l| l == 5,
        );
        assert_eq!(v["event"], "REQUEST_CHANGES");
        assert_eq!(v["comments"].as_array().unwrap().len(), 1);
        assert_eq!(v["comments"][0]["line"], 5);
        assert_eq!(v["comments"][0]["side"], "RIGHT");
        let body = v["body"].as_str().unwrap();
        assert!(body.starts_with("Looks close."));
        assert!(body.contains("`a.rs:100`: far away"), "{body}");
    }

    /// A REAL payload from the REST endpoint, trimmed but not reshaped.
    ///
    /// Captured from `gh api repos/vitali87/croft/pulls/450/comments`. Two
    /// things it establishes that a hand-written fixture cannot: an outdated
    /// comment really does arrive as `line: null` with `original_line` set,
    /// and there is NO `resolved` field on this endpoint at all — resolution
    /// lives only on GraphQL's `reviewThreads.isResolved`. A fixture I
    /// invented would have agreed with my model of the API rather than with
    /// the API.
    #[test]
    fn a_real_rest_payload_parses_as_expected() {
        let json = r#"[{
          "url":"https://api.github.com/repos/vitali87/croft/pulls/comments/3892863850",
          "pull_request_review_id":5064512228,
          "id":3892863850,
          "diff_hunk":"@@ -18,6 +18,7 @@ src/",
          "path":"docs/ARCHITECTURE.md",
          "position":1,
          "original_position":4,
          "line":null,
          "original_line":21,
          "user":{"login":"coderabbitai[bot]"},
          "body":"Describe the current text-result interface."
        }]"#;
        let t = &parse_threads(json)[0];
        assert_eq!(t.path, "docs/ARCHITECTURE.md");
        assert_eq!(t.author, "coderabbitai[bot]");
        assert_eq!(
            t.anchor,
            Anchor::Outdated(20),
            "a null `line` with `original_line` is the outdated case"
        );
        assert!(t.title_for(usize::MAX).contains("outdated"));
        assert!(
            !t.resolved,
            "the REST endpoint carries no resolution state, and unresolved \
             is the safe default"
        );
        // `position` is a DIFF offset, not a file line — using it as one
        // would place this comment at line 0 instead of 20.
        assert_ne!(t.box_line(100), 0);
    }

    /// A thread with no line at all is shown on the file, not dropped.
    ///
    /// A review comment the reviewer never sees is the failure this feature
    /// exists to prevent, so an unplaceable one goes to the top of the file
    /// with a note rather than being filtered out.
    #[test]
    fn a_thread_with_no_line_becomes_a_file_level_box() {
        let json = r#"[
          {"id":3,"path":"src/b.rs","line":null,"original_line":null,
           "user":{"login":"cy"},"body":"whole-file thought"}
        ]"#;
        let threads = parse_threads(json);
        assert_eq!(threads.len(), 1, "an unplaceable thread must not vanish");
        assert_eq!(threads[0].anchor, Anchor::FileLevel);
        assert_eq!(threads[0].box_line(100), 0);
        assert!(threads[0].title_for(usize::MAX).contains("on the file"));
    }

    /// A clamped thread names the line it was really on.
    ///
    /// Two outdated threads from far apart both land on the last line, and
    /// stacked boxes otherwise give no hint their anchors differed at all.
    #[test]
    fn a_clamped_thread_says_where_it_was() {
        let far = Thread {
            id: 1,
            author: String::from("ada"),
            body: String::new(),
            path: String::from("a.rs"),
            anchor: Anchor::Outdated(5000),
            resolved: false,
        };
        let near = Thread {
            anchor: Anchor::Outdated(6000),
            ..far.clone()
        };
        // Both clamp onto the same row of a 100-line file...
        assert_eq!(far.box_line(100), near.box_line(100));
        // ...so the title is the only thing telling them apart.
        assert!(
            far.title_for(100).contains("was line 5001"),
            "{}",
            far.title_for(100)
        );
        assert!(
            near.title_for(100).contains("was line 6001"),
            "{}",
            near.title_for(100)
        );
        assert_ne!(far.title_for(100), near.title_for(100));

        // In a buffer long enough to hold it, the box IS where it says, so
        // repeating the line would be noise.
        assert!(
            !far.title_for(9000).contains("was line"),
            "{}",
            far.title_for(9000)
        );
    }

    /// A CURRENT anchor that is clamped also says where it was.
    ///
    /// Not only an outdated one: the buffer is whatever the user has edited
    /// it to since the threads were loaded, so any line can end up past the
    /// end. Without this the box silently stacks on the last line with a
    /// bare author name, which is the same indistinguishable pile the
    /// outdated case already avoids.
    #[test]
    fn a_clamped_current_thread_also_says_where_it_was() {
        let t = Thread {
            id: 1,
            author: String::from("ada"),
            body: String::from("b"),
            path: String::from("a.rs"),
            anchor: Anchor::At(9),
            resolved: false,
        };
        // Buffer shrank to 3 lines: the anchor is past the end.
        let title = t.title_for(3);
        assert!(
            title.contains("was line 10"),
            "a clamped current anchor must name its line: {title:?}"
        );
        assert!(
            !title.contains("outdated"),
            "it is clamped, not outdated: {title:?}"
        );
        // The separator keeps the title readable rather than leaving a bare
        // comma where the `outdated` clause used to be.
        assert_eq!(title, "ada \u{b7} was line 10", "malformed title");
        // Still inside the buffer: no clamp, so nothing to say.
        assert_eq!(t.title_for(100), "ada");
    }

    /// A line past the end of a shrunken file is clamped into view.
    ///
    /// An outdated line can point past a file that has since lost lines, and
    /// a box at line 4000 of a 100-line file is invisible — which is the
    /// same as losing the comment.
    #[test]
    fn a_line_past_the_end_is_clamped_into_view() {
        let json = r#"[
          {"id":4,"path":"src/c.rs","line":null,"original_line":4000,
           "user":{"login":"dee"},"body":"was here"}
        ]"#;
        let t = &parse_threads(json)[0];
        assert_eq!(t.anchor, Anchor::Outdated(3999));
        assert_eq!(t.box_line(100), 99, "clamped to the last line");
        assert_eq!(t.box_line(0), 0, "an empty buffer has no line to clamp to");
    }

    /// Missing and malformed fields degrade rather than panic or vanish.
    #[test]
    fn a_sparse_or_broken_payload_degrades_gracefully() {
        // No `user` and no `body`: still a thread, with a placeholder author.
        let json = r#"[{"id":5,"path":"src/d.rs","line":3}]"#;
        let t = &parse_threads(json)[0];
        assert_eq!(t.anchor, Anchor::At(2));
        assert_eq!(t.author, "someone");
        assert_eq!(t.body, "");
        assert!(
            !t.resolved,
            "an absent `resolved` must not hide a live thread"
        );

        // No `path` is not about a file, so it cannot become a box.
        assert!(parse_threads(r#"[{"id":6,"line":3}]"#).is_empty());
        // Line 0 does not exist in a 1-based API; treat it as no line.
        let t = &parse_threads(r#"[{"id":7,"path":"x","line":0,"original_line":0}]"#)[0];
        assert_eq!(t.anchor, Anchor::FileLevel);

        // Not JSON, and JSON that is not a list.
        assert!(parse_threads("not json").is_empty());
        assert!(parse_threads(r#"{"message":"Not Found"}"#).is_empty());
        assert!(parse_threads("[]").is_empty());
    }

    /// Threads are filtered to ONE file, and the filter is on the path the
    /// API reports.
    ///
    /// A review's comments span the whole PR. Hanging another file's
    /// objections off this buffer's line numbers puts them against
    /// unrelated code — the same failure as placing an outdated thread
    /// silently, arriving through the other axis. Asserted with two files
    /// whose comments sit at DIFFERENT lines, so a filter that let the
    /// wrong file through would land a box at a line the right file never
    /// had a comment on.
    #[test]
    fn threads_are_filtered_to_the_file_being_viewed() {
        let json = r#"[
          {"id":1,"path":"src/a.rs","line":5,"user":{"login":"ada"},"body":"here"},
          {"id":2,"path":"src/b.rs","line":90,"user":{"login":"bob"},"body":"elsewhere"},
          {"id":3,"path":"src/a.rs","line":7,"user":{"login":"cy"},"body":"also here"}
        ]"#;
        let all = parse_threads(json);
        assert_eq!(all.len(), 3);

        let mine: Vec<&Thread> = all.iter().filter(|t| t.path == "src/a.rs").collect();
        assert_eq!(mine.len(), 2, "only this file's threads");
        assert_eq!(mine[0].anchor, Anchor::At(4));
        assert_eq!(mine[1].anchor, Anchor::At(6));
        assert!(
            !mine.iter().any(|t| t.body == "elsewhere"),
            "another file's comment reached this buffer"
        );
    }

    /// A resolved thread still renders, and says it is resolved.
    #[test]
    fn a_resolved_thread_renders_dimmed_rather_than_hidden() {
        let json = r#"[{"id":8,"path":"x","line":5,"resolved":true,
                        "user":{"login":"eve"},"body":"done"}]"#;
        let t = &parse_threads(json)[0];
        assert!(t.resolved);
        assert!(
            t.title_for(usize::MAX).contains("resolved"),
            "what was already dealt with is part of reading a review: {}",
            t.title_for(usize::MAX)
        );
    }
}
