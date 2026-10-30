//! Seat provenance exported as Git AI authorship notes (`refs/notes/ai`).
//!
//! The provenance overlay (`provenance.rs`) answers "did I write this or did
//! the model?" while the file is open, and a commit forgets the answer: it
//! records who saved, not who typed. The Git AI standard (authorship/3.0.0,
//! `specs/git_ai_standard_v3.0.0.md` in github.com/git-ai-project/git-ai)
//! keeps it in git itself, as one note per commit under `refs/notes/ai` that
//! maps each file's lines to the AI session or known human that wrote them.
//! After every commit croft makes, [`Export::run`] writes that note from the
//! seats local history recorded beside each committed file's last saved
//! snapshot, so `git-ai blame`, its stats, and any other reader of the
//! standard see what croft saw.
//!
//! # Carrying a seat onto a committed line
//!
//! Seats are keyed on the lines of the snapshot croft saved. The note is keyed
//! on the lines of the committed blob, and the two differ whenever only part
//! of a file was staged or something outside croft touched it. So a committed
//! line takes a seat only when a line diff aligns it as EQUAL to a seated
//! snapshot line, and only when the commit added it: git blame answers every
//! other line with an older commit, whose own note owns it. A line that
//! matches nothing is left out, which Git AI reads as "untracked" — croft's
//! Unknown under another name. The provenance invariant holds across the
//! export: no committed line is credited to a seat croft did not watch write
//! that text.
//!
//! # Seats to attestation keys
//!
//! The navigator and an agent pane are AI sessions (`s_…::t_…`). You are a
//! known human (`h_…`), the commit's committer. A collab peer and generated
//! text are left out: a peer may be a person or an MCP agent and croft cannot
//! tell which, and a completion was chosen from a list, not written, by
//! whoever took it.
//!
//! A note goes on only when an AI seat lands in the commit; a commit that is
//! all yours needs none (the standard says as much). A commit that already
//! has a note keeps it: git-ai's own post-commit hook writes a fuller one,
//! with the conversation behind each session, and the standard allows one
//! note per commit.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::Digest as _;

use crate::provenance::{Provenance, Seat};

/// The notes ref the standard reserves (§1.1); `git notes --ref` puts it
/// under `refs/notes/`.
const NOTES_REF: &str = "ai";

/// The schema every log declares (§1.2.1).
const SCHEMA_VERSION: &str = "authorship/3.0.0";

/// The `agent_id.tool` croft's AI seats report: the tool that seated them.
const TOOL: &str = "croft";

/// How long one line diff may run before `similar` settles for a coarser
/// alignment. A coarser alignment only finds fewer EQUAL runs, so a seat is
/// dropped, never moved onto a line it did not write.
const DIFF_BUDGET: Duration = Duration::from_millis(250);

/// Everything the export needs, captured on the UI thread when the commit
/// starts, so the worker that runs it never touches App state.
pub struct Export {
    /// The repository toplevel the commit runs in.
    pub repo_root: PathBuf,
    /// The workspace folder as croft opened it. Local history is keyed on
    /// the path a tab holds, spelled under this folder, which can differ
    /// from git's canonical toplevel by a symlink (macOS's /tmp is one).
    pub workspace: PathBuf,
    /// The local-history root holding the snapshots and their seats.
    pub history_root: PathBuf,
    /// The workspace's pair record, for the model the navigator sits under.
    pub pair_record: PathBuf,
}

/// What the export did with a commit.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No AI seat landed in the commit, so it needs no note.
    Unattributed,
    /// The note went on: so many AI-written lines across so many files.
    Noted { ai_lines: usize, files: usize },
    /// The commit already had a note (git-ai's own hook wrote it); kept.
    AlreadyNoted,
}

impl Outcome {
    /// The Git Output log's line for it.
    pub fn summary(&self) -> String {
        match self {
            Outcome::Unattributed => String::from("no AI-written lines in this commit"),
            Outcome::Noted { ai_lines, files } => format!(
                "{ai_lines} AI-written line{} in {files} file{} noted on refs/notes/{NOTES_REF}",
                if *ai_lines == 1 { "" } else { "s" },
                if *files == 1 { "" } else { "s" },
            ),
            Outcome::AlreadyNoted => {
                format!("the commit already has a note on refs/notes/{NOTES_REF}; kept")
            }
        }
    }
}

impl Export {
    /// Write the note for HEAD, the commit just made. Runs on the commit's
    /// worker thread: two git calls when croft holds no seats for any file
    /// the commit touched, a few per file when it does.
    pub fn run(&self) -> Result<Outcome, String> {
        let root = &self.repo_root;
        let commit = git_text(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        // A merge lists nothing here (no `-m`): its note would cover only
        // the conflict resolution, which croft does not tell apart.
        let changes = parse_name_status(&git_bytes(
            root,
            &[
                "diff-tree",
                "-r",
                "-M",
                "--root",
                "--no-commit-id",
                "-z",
                "--name-status",
                &commit,
            ],
        )?);
        let bases = history_bases(root, &self.workspace);
        let parent = format!("{commit}^");
        let mut files = BTreeMap::new();
        for change in changes {
            let Some((snapshot, seats)) = saved_seats(&self.history_root, &bases, &change.path)
            else {
                continue;
            };
            let Some(committed) = blob(root, &commit, &change.path) else {
                continue;
            };
            let before = change
                .from
                .as_deref()
                .and_then(|from| blob(root, &parent, from));
            let lines = attested_lines(&snapshot, &seats, before.as_deref(), &committed);
            if !lines.is_empty() {
                files.insert(change.path, lines);
            }
        }
        let ai_lines = files.values().flatten().filter(|(_, s)| is_ai(s)).count();
        if ai_lines == 0 {
            return Ok(Outcome::Unattributed);
        }
        let ai_files = files
            .values()
            .filter(|lines| lines.iter().any(|(_, s)| is_ai(s)))
            .count();
        if has_note(root, &commit) {
            return Ok(Outcome::AlreadyNoted);
        }
        let committer = git_text(root, &["show", "-s", "--format=%cn <%ce>", &commit])?;
        let model = crate::session::read_pair_record(&self.pair_record)
            .and_then(|r| r.model)
            .unwrap_or_else(|| String::from("unknown"));
        let log = authorship_log(&commit, &files, &committer, &model);
        add_note(root, &commit, &log)?;
        Ok(Outcome::Noted {
            ai_lines,
            files: ai_files,
        })
    }
}

/// A file the commit added or changed: its path, and its path in the parent
/// (`None` when the commit added it).
#[derive(Debug, PartialEq, Eq)]
struct Change {
    path: String,
    from: Option<String>,
}

/// Parse `diff-tree -z --name-status` into the files that carry lines into
/// the commit. Deletions carry none and are skipped, as is a path that is not
/// UTF-8 (the note and the history keys are both text).
fn parse_name_status(out: &[u8]) -> Vec<Change> {
    let mut fields = out.split(|b| *b == 0).map(|f| std::str::from_utf8(f).ok());
    let mut changes = Vec::new();
    while let Some(Some(status)) = fields.next() {
        // A rename or copy names the path it came from, then the new one.
        let two_paths = status.starts_with('R') || status.starts_with('C');
        let first = fields.next().flatten();
        let (from, path) = if two_paths {
            (first, fields.next().flatten())
        } else {
            (first, first)
        };
        let Some(path) = path else {
            continue;
        };
        let from = match status.chars().next() {
            Some('A') => None,
            Some('D') | None => continue,
            Some(_) => from.map(str::to_string),
        };
        changes.push(Change {
            path: path.to_string(),
            from,
        });
    }
    changes
}

/// The spellings of the repository root that local-history keys may be under:
/// git's toplevel, and the same folder spelled through the workspace path
/// croft opened, which a symlink can make different.
fn history_bases(repo_root: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut bases = vec![repo_root.to_path_buf()];
    let Ok(canon) = std::fs::canonicalize(workspace) else {
        return bases;
    };
    let base = if let Ok(below) = canon.strip_prefix(repo_root) {
        workspace
            .ancestors()
            .nth(below.components().count())
            .map(Path::to_path_buf)
    } else {
        repo_root
            .strip_prefix(&canon)
            .ok()
            .map(|inside| workspace.join(inside))
    };
    if let Some(base) = base
        && !bases.contains(&base)
    {
        bases.push(base);
    }
    bases
}

/// The newest saved snapshot of `rel` and the seats recorded beside it, from
/// the first spelling of the root that has any. A snapshot with no seats, or
/// text that is not UTF-8, gives nothing to carry.
fn saved_seats(history_root: &Path, bases: &[PathBuf], rel: &str) -> Option<(String, Provenance)> {
    bases.iter().find_map(|base| {
        let abs = base.join(rel);
        let newest = crate::history::entries_in(history_root, &abs)
            .into_iter()
            .next()?;
        let seats = crate::history::seats_for(history_root, &abs, newest.millis)?;
        let text = String::from_utf8(std::fs::read(&newest.file).ok()?).ok()?;
        Some((text, seats))
    })
}

/// The committed lines a seat is carried onto, 1-based and ascending: lines
/// the commit added (against `before`, the file in the parent) that align
/// EQUAL to a snapshot line wearing an exported seat.
fn attested_lines(
    snapshot: &str,
    seats: &Provenance,
    before: Option<&str>,
    committed: &str,
) -> Vec<(usize, Seat)> {
    let committed_lines: Vec<&str> = committed.lines().collect();
    let added = added_lines(before, &committed_lines);
    let snapshot_lines: Vec<&str> = snapshot.lines().collect();
    let mut out = Vec::new();
    for (old, new) in equal_lines(&snapshot_lines, &committed_lines) {
        if let Some(seat) = seats.seat(old)
            && is_exported(seat)
            && added.contains(&new)
        {
            out.push((new + 1, seat.clone()));
        }
    }
    out
}

/// The 0-based lines of `committed` the commit added or changed against
/// `before`; every line when the commit added the file.
fn added_lines(before: Option<&str>, committed: &[&str]) -> BTreeSet<usize> {
    let Some(before) = before else {
        return (0..committed.len()).collect();
    };
    let before: Vec<&str> = before.lines().collect();
    let mut added = BTreeSet::new();
    for op in line_ops(&before, committed) {
        let (tag, _, new) = op.as_tag_tuple();
        if matches!(tag, similar::DiffTag::Insert | similar::DiffTag::Replace) {
            added.extend(new);
        }
    }
    added
}

/// Every `(old, new)` pair of 0-based line indices a line diff aligns as
/// equal, in order.
fn equal_lines(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for op in line_ops(old, new) {
        let (tag, old_range, new_range) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            pairs.extend(old_range.zip(new_range));
        }
    }
    pairs
}

/// One line diff under [`DIFF_BUDGET`]. Lines are compared without their
/// endings, so a file checked out with CRLF still aligns with its saved text.
fn line_ops(old: &[&str], new: &[&str]) -> Vec<similar::DiffOp> {
    similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        old,
        new,
        Some(Instant::now() + DIFF_BUDGET),
    )
}

/// A seat the note can name truthfully (see the module docs).
fn is_exported(seat: &Seat) -> bool {
    matches!(seat, Seat::Me | Seat::Navigator | Seat::Agent(_))
}

/// A seat that is an AI session rather than a person.
fn is_ai(seat: &Seat) -> bool {
    matches!(seat, Seat::Navigator | Seat::Agent(_))
}

/// The `agent_id` of an AI seat: its conversation id in croft's domain and
/// the model behind it. Seats record which seat wrote a line, not which model
/// sat in it, so the navigator reports the model it is configured with now,
/// and an agent pane, whose model croft never learns, reports `unknown`.
fn agent_id(seat: &Seat, model: &str) -> (String, String) {
    match seat {
        Seat::Agent(pane) => (format!("agent:{pane}"), String::from("unknown")),
        _ => (String::from("navigator"), model.to_string()),
    }
}

/// The first 14 hex digits of SHA-256 over `input`, the standard's hash for
/// session, trace and human keys (§1.2.3.1).
fn short_hash(input: &str) -> String {
    let digest = format!("{:x}", sha2::Sha256::digest(input.as_bytes()));
    digest[..14].to_string()
}

/// A session's key: `s_` + SHA-256 of `<tool>:<conversation id>`, stable
/// across commits so every note names one navigator the same way.
fn session_key(conversation: &str) -> String {
    format!("s_{}", short_hash(&format!("{TOOL}:{conversation}")))
}

/// A trace key, one per session per commit. The standard asks only that
/// each checkpoint's be unique; deriving it from the commit makes it so and
/// keeps a re-run of the same export byte-identical.
fn trace_key(commit: &str, session: &str) -> String {
    format!("t_{}", short_hash(&format!("{commit}:{session}")))
}

/// A known human's key: `h_` + SHA-256 of the committer identity.
fn human_key(identity: &str) -> String {
    format!("h_{}", short_hash(identity))
}

/// A path as the attestation section writes it: quoted when it holds a
/// space, tab or newline (§1.2.3).
fn quote_path(path: &str) -> String {
    if path.contains([' ', '\t', '\n']) {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}

/// Ascending 1-based lines as a range list: `1-4,9,12-13`.
fn ranges(lines: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = lines[i];
        let mut end = start;
        while i + 1 < lines.len() && lines[i + 1] == end + 1 {
            i += 1;
            end = lines[i];
        }
        out.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        i += 1;
    }
    out.join(",")
}

/// The authorship log for `commit`: the attestation section, the `---`
/// divider, and the metadata JSON (§1.2). `files` maps each path to its
/// attested lines (1-based, ascending); `committer` is `Name <email>`.
fn authorship_log(
    commit: &str,
    files: &BTreeMap<String, Vec<(usize, Seat)>>,
    committer: &str,
    model: &str,
) -> String {
    let email = committer
        .rsplit_once('<')
        .and_then(|(_, rest)| rest.strip_suffix('>'))
        .unwrap_or(committer);
    let mut sessions = serde_json::Map::new();
    let mut humans = serde_json::Map::new();
    let mut out = String::new();
    for (path, lines) in files {
        let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (line, seat) in lines {
            let key = if is_ai(seat) {
                let (conversation, model) = agent_id(seat, model);
                let session = session_key(&conversation);
                sessions.entry(session.clone()).or_insert_with(|| {
                    serde_json::json!({
                        "agent_id": { "tool": TOOL, "id": conversation, "model": model },
                        "human_author": email,
                    })
                });
                format!("{session}::{}", trace_key(commit, &session))
            } else {
                let human = human_key(committer);
                humans
                    .entry(human.clone())
                    .or_insert_with(|| serde_json::json!({ "author": committer }));
                human
            };
            by_key.entry(key).or_default().push(*line);
        }
        out.push_str(&quote_path(path));
        out.push('\n');
        for (key, lines) in by_key {
            out.push_str(&format!("  {key} {}\n", ranges(&lines)));
        }
    }
    out.push_str("---\n");
    let mut meta = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "base_commit_sha": commit,
        "prompts": {},
        "sessions": sessions,
    });
    if !humans.is_empty() {
        meta["humans"] = serde_json::Value::Object(humans);
    }
    out.push_str(&serde_json::to_string_pretty(&meta).unwrap_or_default());
    out.push('\n');
    out
}

/// `git <args>` in `root`, its stdout as bytes, or its stderr as the error.
fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// [`git_bytes`] as trimmed text.
fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    git_bytes(root, args).map(|out| String::from_utf8_lossy(&out).trim().to_string())
}

/// The UTF-8 text of `<rev>:<path>`, or `None` when it is missing or binary.
fn blob(root: &Path, rev: &str, path: &str) -> Option<String> {
    let bytes = git_bytes(root, &["cat-file", "blob", &format!("{rev}:{path}")]).ok()?;
    String::from_utf8(bytes).ok()
}

/// Whether `commit` already carries a note on the AI notes ref. Asked by
/// exit status rather than by matching `git notes add`'s refusal, whose
/// wording follows the user's locale.
fn has_note(root: &Path, commit: &str) -> bool {
    git_bytes(
        root,
        &["notes", &format!("--ref={NOTES_REF}"), "list", commit],
    )
    .is_ok()
}

/// Attach `log` to `commit` on the AI notes ref, never over an existing note.
fn add_note(root: &Path, commit: &str, log: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "notes",
            &format!("--ref={NOTES_REF}"),
            "add",
            "-F",
            "-",
            commit,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(log.as_bytes())
            .map_err(|e| format!("writing the note to git: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("waiting on git notes: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The range list the attestation section uses: runs collapse, singles
    /// stay single, and nothing carries a space.
    #[test]
    fn lines_collapse_into_the_standards_range_list() {
        assert_eq!(ranges(&[1, 2, 3, 4, 9, 12, 13]), "1-4,9,12-13");
        assert_eq!(ranges(&[7]), "7");
        assert_eq!(ranges(&[]), "");
    }

    /// Keys follow §1.2.3.1: a prefix, 14 hex digits, and for a session a
    /// `::t_` trace. A session's key is the same in every commit; its trace
    /// is not.
    #[test]
    fn keys_have_the_standards_shape() {
        let s = session_key("navigator");
        assert!(s.starts_with("s_") && s.len() == 16, "{s}");
        assert!(s[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s, session_key("navigator"), "stable across commits");
        assert_ne!(trace_key("aaa", &s), trace_key("bbb", &s), "one per commit");
        let t = trace_key("aaa", &s);
        assert!(t.starts_with("t_") && t.len() == 16, "{t}");
        let h = human_key("Ada <ada@example.com>");
        assert!(h.starts_with("h_") && h.len() == 16, "{h}");
        // §1.2.3.1's own definition, spelled out.
        let want = format!("{:x}", sha2::Sha256::digest(b"croft:navigator"));
        assert_eq!(s, format!("s_{}", &want[..14]));
    }

    /// A whole log: files sorted, keys indented two spaces, the bare `---`
    /// divider, and metadata that names every key the section uses.
    #[test]
    fn the_log_has_an_attestation_section_a_divider_and_metadata() {
        let mut files = BTreeMap::new();
        files.insert(
            String::from("src/main.rs"),
            vec![
                (1, Seat::Navigator),
                (2, Seat::Navigator),
                (4, Seat::Me),
                (5, Seat::Navigator),
            ],
        );
        files.insert(String::from("my notes.md"), vec![(3, Seat::Navigator)]);
        let log = authorship_log("c0ffee", &files, "Ada <ada@example.com>", "qwen3-coder:30b");
        let (section, meta) = log.split_once("\n---\n").expect("the divider");
        let session = session_key("navigator");
        let trace = trace_key("c0ffee", &session);
        let human = human_key("Ada <ada@example.com>");
        assert_eq!(
            section,
            format!(
                "\"my notes.md\"\n  {session}::{trace} 3\nsrc/main.rs\n  {human} 4\n  {session}::{trace} 1-2,5"
            )
        );
        let meta: serde_json::Value = serde_json::from_str(meta).unwrap();
        assert_eq!(meta["schema_version"], "authorship/3.0.0");
        assert_eq!(meta["base_commit_sha"], "c0ffee");
        assert_eq!(meta["prompts"], serde_json::json!({}));
        assert_eq!(
            meta["sessions"][&session],
            serde_json::json!({
                "agent_id": { "tool": "croft", "id": "navigator", "model": "qwen3-coder:30b" },
                "human_author": "ada@example.com",
            })
        );
        assert_eq!(
            meta["humans"][&human],
            serde_json::json!({ "author": "Ada <ada@example.com>" })
        );
    }

    /// The invariant, across the export: a committed line takes a seat only
    /// when it is the snapshot's line AND the commit added it. A line the
    /// commit kept, a line changed after the save, and a seat the note cannot
    /// name truthfully all stay out.
    #[test]
    fn a_seat_reaches_only_added_lines_that_match_the_saved_text() {
        let snapshot = "keep\nnav one\nnav two\nmine\npeer\n";
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Navigator); // kept from the parent
        seats.record(1..3, Seat::Navigator);
        seats.record(3..4, Seat::Me);
        seats.record(4..5, Seat::Peer(String::from("ada")));
        let before = "keep\n";
        // `nav two` was edited outside croft after the save.
        let committed = "keep\nnav one\nNAV TWO\nmine\npeer\n";
        assert_eq!(
            attested_lines(snapshot, &seats, Some(before), committed),
            vec![(2, Seat::Navigator), (4, Seat::Me)]
        );
        // A file the commit added: every matching line is new.
        assert_eq!(
            attested_lines(snapshot, &seats, None, committed),
            vec![(1, Seat::Navigator), (2, Seat::Navigator), (4, Seat::Me)]
        );
    }

    /// Partial staging: the commit holds only some of the saved lines, at
    /// other indices. The seats follow their text, not their index.
    #[test]
    fn seats_follow_their_text_into_a_partly_staged_commit() {
        let snapshot = "a\nnav\nb\nlater\n";
        let mut seats = Provenance::new();
        seats.record(1..2, Seat::Navigator);
        seats.record(3..4, Seat::Navigator);
        // `later` stayed unstaged; a line the parent had sits above.
        let before = "top\na\nb\n";
        let committed = "top\na\nnav\nb\n";
        assert_eq!(
            attested_lines(snapshot, &seats, Some(before), committed),
            vec![(3, Seat::Navigator)]
        );
    }

    /// CRLF in the commit against LF in the saved text still aligns.
    #[test]
    fn line_endings_do_not_break_the_alignment() {
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Navigator);
        assert_eq!(
            attested_lines("nav\n", &seats, None, "nav\r\n"),
            vec![(1, Seat::Navigator)]
        );
    }

    /// `diff-tree -z --name-status`: adds have no parent path, renames keep
    /// the old one, deletions carry nothing.
    #[test]
    fn name_status_names_each_file_and_where_it_came_from() {
        let out = b"A\0new.rs\0M\0src/lib.rs\0R087\0old name.rs\0new name.rs\0D\0gone.rs\0";
        assert_eq!(
            parse_name_status(out),
            vec![
                Change {
                    path: String::from("new.rs"),
                    from: None
                },
                Change {
                    path: String::from("src/lib.rs"),
                    from: Some(String::from("src/lib.rs"))
                },
                Change {
                    path: String::from("new name.rs"),
                    from: Some(String::from("old name.rs"))
                },
            ]
        );
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.name", "Ada"]);
        git(tmp.path(), &["config", "user.email", "ada@example.com"]);
        tmp
    }

    fn export(repo: &Path, history: &Path) -> Export {
        Export {
            repo_root: PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"]).trim()),
            workspace: repo.to_path_buf(),
            history_root: history.to_path_buf(),
            pair_record: repo.join("no-pair-record.json"),
        }
    }

    /// End to end in a real repo: the saved seats become a note on the new
    /// commit under refs/notes/ai, readable with plain `git notes`, and a
    /// second export leaves the note alone.
    #[test]
    fn a_commit_with_navigator_lines_gets_a_git_ai_note() {
        let tmp = repo();
        let history = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f.rs");
        std::fs::write(&f, "fn alpha() {}\n").unwrap();
        git(tmp.path(), &["add", "f.rs"]);
        git(tmp.path(), &["commit", "-q", "-m", "alpha"]);
        let text = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
        std::fs::write(&f, text).unwrap();
        let mut seats = Provenance::new();
        seats.record(1..2, Seat::Navigator);
        seats.record(2..3, Seat::Me);
        crate::history::record_with_seats_in(history.path(), &f, text.as_bytes(), 1_000, &seats)
            .unwrap();
        git(tmp.path(), &["commit", "-q", "-am", "beta"]);

        let job = export(tmp.path(), history.path());
        assert_eq!(
            job.run(),
            Ok(Outcome::Noted {
                ai_lines: 1,
                files: 1
            })
        );
        let note = git(tmp.path(), &["notes", "--ref=ai", "show", "HEAD"]);
        let head = git(tmp.path(), &["rev-parse", "HEAD"]).trim().to_string();
        let session = session_key("navigator");
        let human = human_key("Ada <ada@example.com>");
        assert!(
            note.starts_with(&format!(
                "f.rs\n  {human} 3\n  {session}::{} 2\n---\n",
                trace_key(&head, &session)
            )),
            "{note}"
        );
        assert!(
            note.contains(&format!("\"base_commit_sha\": \"{head}\"")),
            "{note}"
        );
        assert!(note.contains("\"model\": \"unknown\""), "{note}");
        // The first commit's line 1 is not this commit's to claim.
        assert!(
            git(tmp.path(), &["notes", "--ref=ai", "list"])
                .lines()
                .count()
                == 1,
            "one note, on HEAD"
        );
        assert_eq!(job.run(), Ok(Outcome::AlreadyNoted), "never overwritten");
    }

    /// A commit with no AI seat in it gets no note: neither your own lines
    /// nor a file croft never saw is a reason to write to refs/notes/ai.
    #[test]
    fn a_commit_without_ai_lines_gets_no_note() {
        let tmp = repo();
        let history = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f.txt");
        std::fs::write(&f, "mine\n").unwrap();
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Me);
        crate::history::record_with_seats_in(history.path(), &f, b"mine\n", 1_000, &seats).unwrap();
        std::fs::write(tmp.path().join("other.txt"), "unseen\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-q", "-m", "mine"]);
        assert_eq!(
            export(tmp.path(), history.path()).run(),
            Ok(Outcome::Unattributed)
        );
        assert_eq!(git(tmp.path(), &["notes", "--ref=ai", "list"]), "");
    }
}
