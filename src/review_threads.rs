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
    items.iter().filter_map(thread_from).collect()
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
            t.push_str(&format!(", was line {}", line + 1));
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
