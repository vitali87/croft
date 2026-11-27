//! Blame: which commit last changed each line of a file.

use super::*;

/// The commit that last touched one or more lines, for GitLens-style inline
/// blame. Lines not yet committed (staged or unstaged working-tree edits)
/// blame against the all-zero hash, which [`parse_blame`] marks
/// `uncommitted`.
#[derive(Debug, PartialEq, Eq)]
pub struct BlameCommit {
    /// The full hash, which Git: Copy Commit Hash for Line copies and Git:
    /// Open Commit for Line shows (#1556).
    pub hash: String,
    pub summary: String,
    pub author: String,
    /// Seconds since the commit's author-time, for [`humanize_age`].
    pub age_secs: i64,
    /// True for a not-yet-committed line (the zero hash `git blame` reports
    /// for working-tree changes).
    pub uncommitted: bool,
}

impl BlameCommit {
    /// The hash's first eight digits, as the inline blame names a commit.
    pub fn short_hash(&self) -> &str {
        &self.hash[..self.hash.len().min(8)]
    }
}

/// A file's blame, line by line (#1707). Each line's commit is shared with
/// every other line it last touched, and the lines' text is packed: a file
/// names far fewer commits than it has lines, and a copy of the hash,
/// author, summary and text on every line cost ~330 bytes a line, 23 MB for
/// a 73,000-line file, in every tab blame had visited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    commits: Vec<Arc<BlameCommit>>,
    /// The lines' text as blamed, when known: the annotation is shown only
    /// while the buffer's line still reads the same (see
    /// `Editor::committed_blame_annotation`).
    text: Option<crate::packed_lines::PackedLines>,
}

impl Blame {
    /// Blame for lines whose commits are `commits`, in order, and whose
    /// text as blamed is `text` when known.
    pub fn new(
        commits: Vec<Arc<BlameCommit>>,
        text: Option<crate::packed_lines::PackedLines>,
    ) -> Self {
        Self { commits, text }
    }

    /// How many lines were blamed.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    /// Whether no line was blamed (git failed, or the file is untracked).
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// Line `i`'s commit, and its text as blamed when known.
    pub fn line(&self, i: usize) -> Option<(&BlameCommit, Option<&str>)> {
        let commit = self.commits.get(i)?;
        Some((commit, self.text.as_ref().and_then(|t| t.get(i))))
    }
}

/// Per-line blame for `rel_path`, indexed 0-based by result position (result
/// `[0]` is line 1). Uses `git blame --porcelain`, which describes each
/// commit once, where `--line-porcelain` repeated its author, time and
/// summary on every line it touched: several times the output to read for
/// the same answer. Returns empty on any failure so the caller renders no
/// annotation rather than an error.
pub fn blame(root: &Path, rel_path: &str) -> Blame {
    let (dir, name) = file_cwd(root, rel_path);
    let Some(path_str) = dir.to_str() else {
        return Blame::default();
    };
    let output = Git::new()
        .without_optional_locks()
        .at(path_str)
        .args(["blame", "--porcelain", "--", &name])
        .into_command()
        .output();
    let Ok(output) = output else {
        return Blame::default();
    };
    if !output.status.success() {
        return Blame::default();
    }
    parse_blame(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// Parse `git blame --porcelain` output into the [`Blame`] of each source
/// line, in order. Porcelain groups each line as a header (`<hash> <orig>
/// <final> [group]`), then the commit's `author`, `author-time`, `summary`,
/// etc. the first time that commit appears, then a tab-prefixed content
/// line. A later line of the same commit shares the first one's
/// [`BlameCommit`]; `--line-porcelain`'s repeated metadata parses the same.
/// `now` (unix seconds) dates each commit.
pub fn parse_blame(out: &str, now: i64) -> Blame {
    let mut lines = Vec::new();
    let mut text = crate::packed_lines::PackedLines::default();
    let mut commits: std::collections::HashMap<String, Arc<BlameCommit>> =
        std::collections::HashMap::new();
    let mut hash: Option<String> = None;
    let mut author = String::new();
    let mut summary = String::new();
    let mut author_time: i64 = now;
    for raw in out.lines() {
        if let Some(rest) = raw.strip_prefix("author ") {
            author = rest.to_string();
        } else if let Some(rest) = raw.strip_prefix("author-time ") {
            author_time = rest.trim().parse().unwrap_or(now);
        } else if let Some(rest) = raw.strip_prefix("summary ") {
            summary = rest.to_string();
        } else if let Some(content) = raw.strip_prefix('\t') {
            // The content line closes a group: emit the accumulated blame.
            if let Some(full) = hash.take() {
                let commit = commits.entry(full).or_insert_with_key(|full| {
                    Arc::new(BlameCommit {
                        hash: full.clone(),
                        summary: std::mem::take(&mut summary),
                        author: std::mem::take(&mut author),
                        age_secs: now - author_time,
                        uncommitted: full.chars().all(|c| c == '0'),
                    })
                });
                lines.push(Arc::clone(commit));
                text.push(content);
            }
        } else if let Some(h) = raw.split(' ').next() {
            // A header line begins with a 40-char hash; other porcelain
            // metadata (previous/filename/committer/…) is ignored. A group's
            // metadata is only what follows its own header.
            if h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()) {
                hash = Some(h.to_string());
                author.clear();
                summary.clear();
                author_time = now;
            }
        }
    }
    Blame::new(lines, Some(text))
}
