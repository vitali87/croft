use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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
    /// The HEAD commit's full oid, or `None` outside a repo. Drives git-gutter
    /// baseline invalidation: when this moves (commit, checkout, pull) the
    /// cached HEAD file snapshots are stale and must be refetched.
    pub head_oid: Option<String>,
    /// Absolute paths of git-ignored files and directories, so the Explorer
    /// can grey them out (VS Code's ignored-resource decoration). A fully
    /// ignored directory appears as one collapsed entry — descendants are
    /// resolved by walking ancestors, never enumerated. Arc: the status is
    /// cloned per consumer each refresh and the set can be large.
    pub ignored: Arc<HashSet<PathBuf>>,
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
    let (ahead, behind) = match run_git(
        root,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    ) {
        Ok(s) => parse_ahead_behind(&s),
        Err(_) => (0, 0),
    };
    let head_oid = run_git(root, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    GitStatus {
        in_repo: true,
        branch,
        detached_hash,
        dirty,
        ahead,
        behind,
        head_oid,
        ignored: Arc::new(query_ignored(root)),
    }
}

/// The git-ignored paths under `root`, absolute.
///
/// Two-step, because neither step alone is correct. `ls-files --directory`
/// gives a cheap candidate set (a fully-ignored directory collapses to one
/// entry instead of enumerating `target/`'s thousands of files), but it also
/// collapses any ENTIRELY UNTRACKED directory whose contents all happen to be
/// ignored — even when no rule matches the directory itself, so `logs/` shows
/// up merely because `logs/app.log` is ignored. `check-ignore` then answers
/// per path, exactly the per-resource question VS Code asks, and only its
/// verdicts reach the set.
///
/// Both calls go through [`git_raw`]: filenames may carry leading or trailing
/// whitespace (git sorts a leading-space name FIRST, so a blanket `trim()`
/// eats it) or bytes that are not UTF-8, so the `-z` output is split on NUL
/// and converted without lossy decoding.
fn query_ignored(root: &Path) -> HashSet<PathBuf> {
    let listed = git_raw(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--directory",
            "--exclude-standard",
        ],
        None,
    )
    .unwrap_or_default();
    let mut stdin = Vec::with_capacity(listed.len());
    for candidate in listed.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        stdin.extend_from_slice(candidate);
        stdin.push(0);
    }
    if stdin.is_empty() {
        return HashSet::new();
    }
    // check-ignore exits 1 when nothing matches: a verdict, not a failure, so
    // `git_raw` deliberately does not gate on the exit status.
    let confirmed = git_raw(root, &["check-ignore", "-z", "--stdin"], Some(&stdin)).unwrap_or_default();
    confirmed
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|raw| join_raw(root, raw))
        .collect()
}

/// Join a raw `-z` path (trailing `/` stripped) onto `root` without passing
/// through `str`: a filename is bytes, and on unix it need not be UTF-8.
fn join_raw(root: &Path, raw: &[u8]) -> PathBuf {
    let trimmed = raw.strip_suffix(b"/").unwrap_or(raw);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(trimmed))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(trimmed).as_ref())
    }
}

/// Spawn `git` under `root` and hand back stdout as RAW BYTES, optionally
/// feeding `stdin_data` first. Unlike [`run_git`] this neither trims nor
/// UTF-8-decodes, and it ignores the exit status — the callers here read
/// NUL-separated filenames, and `check-ignore` uses exit 1 as an answer.
fn git_raw(root: &Path, args: &[&str], stdin_data: Option<&[u8]>) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(if stdin_data.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
    let mut child = cmd.spawn().ok()?;
    if let Some(data) = stdin_data {
        // Dropping the handle closes the pipe, which is what makes
        // check-ignore stop reading and produce its answer.
        child.stdin.take()?.write_all(data).ok()?;
    }
    Some(child.wait_with_output().ok()?.stdout)
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
    // Read-only queries must never take `.git/index.lock`: `git status` /
    // `git diff` opportunistically rewrite the refreshed index, and that
    // background lock races user-initiated mutations (`git add`, `git
    // apply --cached`) into transient "index.lock exists" failures. This
    // is exactly what git ships GIT_OPTIONAL_LOCKS for.
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
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

/// Run a mutating git subcommand and return a human-readable summary.
///
/// Shared by every operation the Source Control panel invokes
/// synchronously (unstage, pull, sync, branch switch/create, stash) so
/// they don't each re-implement the spawn → check → pick-the-right-stream
/// dance. On success git often reports the interesting line on *stderr*
/// (push/pull print "Everything up-to-date" / ref updates there), so we
/// surface stderr when stdout is empty and vice-versa. On failure we
/// surface whichever stream carries the message verbatim, so the panel
/// shows the host's exact reason (e.g. "fatal: 'x' is not a commit").
fn run_mutation(root: &Path, args: &[&str]) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    // Echo the command and its result into the OUTPUT panel's "Git" channel, so
    // the user can see exactly what the Source Control panel ran (VS Code's Git
    // output channel). Only mutations are logged here; status polls stay quiet.
    crate::output::push(
        crate::output::CHANNEL_GIT,
        crate::output::OutputLevel::Info,
        &format!("> git {}", args.join(" ")),
    );
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str]).args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let msg = if !stdout.is_empty() { stdout } else { stderr };
        if !msg.is_empty() {
            crate::output::push(
                crate::output::CHANNEL_GIT,
                crate::output::OutputLevel::Info,
                &msg,
            );
        }
        Ok(msg)
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        let err = if msg.is_empty() {
            let verb = args.first().copied().unwrap_or("git");
            format!("git {verb} failed with code {:?}", output.status.code())
        } else {
            msg
        };
        crate::output::push(
            crate::output::CHANNEL_GIT,
            crate::output::OutputLevel::Error,
            &err,
        );
        Err(err)
    }
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
    pub additions: usize,
    pub deletions: usize,
}

impl Default for ChangeEntry {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: ChangeKind::Modified,
            additions: 0,
            deletions: 0,
        }
    }
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

/// Undo the C-style quoting `git status --porcelain` applies to paths
/// containing unusual characters (space, control chars, double-quote,
/// backslash, or — when `core.quotepath = true`, which is the default —
/// any non-ASCII byte). Plain ASCII paths pass through unchanged.
///
/// Quoted form: surrounded by `"`, with `\a \b \t \n \v \f \r \" \\`
/// single-char escapes and `\NNN` 3-digit octal escapes for any other
/// byte. Octal escapes are collected into a byte buffer first so that
/// multi-byte UTF-8 sequences (e.g. `\303\244` for `ä`) decode back to
/// the original character.
pub fn unquote_porcelain_path(raw: &str) -> String {
    if !raw.starts_with('"') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i: usize = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            break;
        }
        if b == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            match n {
                b'a' => {
                    out.push(0x07);
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'v' => {
                    out.push(0x0B);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'0'..=b'7' => {
                    // Consume a run of `\NNN` triplets so multi-byte
                    // UTF-8 sequences land in `out` as raw bytes ready
                    // for the lossy decode below.
                    while i + 3 < bytes.len()
                        && bytes[i] == b'\\'
                        && (b'0'..=b'7').contains(&bytes[i + 1])
                        && (b'0'..=b'7').contains(&bytes[i + 2])
                        && (b'0'..=b'7').contains(&bytes[i + 3])
                    {
                        let val = ((bytes[i + 1] - b'0') as u16) * 64
                            + ((bytes[i + 2] - b'0') as u16) * 8
                            + ((bytes[i + 3] - b'0') as u16);
                        out.push(val as u8);
                        i += 4;
                    }
                }
                _ => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `git status --porcelain` (v1) into the change entries the Source
/// Control panel renders. Each line is `XY <path>` where `X` is the index
/// status and `Y` is the worktree status; renames carry both old and new
/// paths separated by ` -> `. Paths with special characters arrive in
/// C-style quoted form — see `unquote_porcelain_path`.
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
            unquote_porcelain_path(dst)
        } else {
            unquote_porcelain_path(path_part)
        };
        // Conflicts: AA, DD, AU, UA, DU, UD, UU.
        let conflict = matches!(
            (x, y),
            ('A', 'A')
                | ('D', 'D')
                | ('A', 'U')
                | ('U', 'A')
                | ('D', 'U')
                | ('U', 'D')
                | ('U', 'U')
        );
        if conflict {
            entries.push(ChangeEntry {
                path,
                kind: ChangeKind::Conflicted,
                ..Default::default()
            });
            continue;
        }
        if x == '?' && y == '?' {
            entries.push(ChangeEntry {
                path,
                kind: ChangeKind::Untracked,
                ..Default::default()
            });
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
            entries.push(ChangeEntry {
                path: path.clone(),
                kind,
                ..Default::default()
            });
        }
        if y != ' ' && y != '?' {
            let kind = match y {
                'M' => ChangeKind::Modified,
                'D' => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            entries.push(ChangeEntry {
                path,
                kind,
                ..Default::default()
            });
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
        // Poll without taking index.lock; see run_git.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", path_str, "status", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut entries = parse_porcelain_changes(&raw);
    let staged = numstat_map(path_str, &["diff", "--cached", "--numstat"]);
    let unstaged = numstat_map(path_str, &["diff", "--numstat"]);
    for entry in &mut entries {
        let stats = match entry.kind.section() {
            ChangeSection::Staged => staged.get(&entry.path),
            ChangeSection::Changes => unstaged.get(&entry.path),
            ChangeSection::Untracked => None,
            ChangeSection::Conflicts => None,
        };
        if let Some((a, d)) = stats {
            entry.additions = *a;
            entry.deletions = *d;
        }
    }
    for entry in &mut entries {
        if matches!(entry.kind, ChangeKind::Untracked) {
            entry.additions = count_lines_in_file(root, &entry.path);
        }
    }
    entries
}

/// Parse `git diff --numstat` output into a path→(additions, deletions)
/// map. Each line is `\d+\t\d+\t<path>` for text diffs, or `-\t-\t<path>`
/// for binary files (which we map to (0, 0) — there's no meaningful line
/// count). Rename lines arrive as `add\tdel\told -> new` and we key on
/// the destination path so the lookup matches what porcelain emitted.
pub fn parse_numstat(out: &str) -> std::collections::HashMap<String, (usize, usize)> {
    let mut map = std::collections::HashMap::new();
    for line in out.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(adds) = parts.next() else { continue };
        let Some(dels) = parts.next() else { continue };
        let Some(path_part) = parts.next() else {
            continue;
        };
        let additions = adds.parse::<usize>().unwrap_or(0);
        let deletions = dels.parse::<usize>().unwrap_or(0);
        let path = if let Some((_, dst)) = path_part.split_once(" -> ") {
            unquote_porcelain_path(dst)
        } else {
            unquote_porcelain_path(path_part)
        };
        map.insert(path, (additions, deletions));
    }
    map
}

fn numstat_map(workdir: &str, args: &[&str]) -> std::collections::HashMap<String, (usize, usize)> {
    let output = match Command::new("git")
        // Poll without taking index.lock; see run_git.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", workdir])
        .args(args)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashMap::new(),
    };
    parse_numstat(&String::from_utf8_lossy(&output.stdout))
}

fn count_lines_in_file(root: &Path, rel_path: &str) -> usize {
    let abs = root.join(rel_path);
    let Ok(bytes) = std::fs::read(&abs) else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    let mut lines = bytes.iter().filter(|b| **b == b'\n').count();
    if !bytes.ends_with(b"\n") {
        lines += 1;
    }
    lines
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
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let spec = format!("HEAD:{rel_path}");
    let output = Command::new("git")
        .args(["-C", path_str, "show", &spec])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "git show {spec} failed with code {:?}",
                output.status.code()
            )
        } else {
            stderr
        });
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{rel_path} at HEAD is not UTF-8"))
}

/// Apply a unified-diff patch fed on stdin via `git apply`. `cached`
/// targets the index (stage), `reverse` un-applies (`cached + reverse` =
/// unstage, `reverse` alone = revert the working tree). Logged to the
/// OUTPUT panel's Git channel like every other mutation; the caller gets
/// git's verbatim error when a hunk no longer applies.
pub fn apply_patch(
    root: &Path,
    patch: &str,
    cached: bool,
    reverse: bool,
) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let mut args: Vec<&str> = vec!["-C", path_str, "apply", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.push("-");
    crate::output::push(
        crate::output::CHANNEL_GIT,
        crate::output::OutputLevel::Info,
        &format!("> git {} <<patch", args[2..].join(" ")),
    );
    let mut child = Command::new("git")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if let Some(stdin) = child.stdin.take() {
        use std::io::Write;
        let mut stdin = stdin;
        if let Err(e) = stdin.write_all(patch.as_bytes()) {
            // Reap before bailing or the failed child lingers as a zombie.
            drop(stdin);
            let _ = child.wait();
            return Err(format!("failed to feed patch to git apply: {e}"));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git apply did not exit cleanly: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if !stdout.is_empty() { stdout } else { stderr })
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        let err = if msg.is_empty() {
            format!("git apply failed with code {:?}", output.status.code())
        } else {
            msg
        };
        crate::output::push(
            crate::output::CHANNEL_GIT,
            crate::output::OutputLevel::Error,
            &err,
        );
        Err(err)
    }
}

/// message verbatim so the user sees exactly what blocked them.
pub fn commit_all_tracked(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
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

/// Push the current branch to its upstream. Used by Commit & Push
/// (Cmd+Enter in the SC message box). Returns the trimmed stdout/stderr
/// summary verbatim so the panel can surface the host's full message —
/// `git push` typically reports the ref update on stderr even on success.
pub fn push_current_branch(root: &Path) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let output = Command::new("git")
        .args(["-C", path_str, "push"])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let summary = if !stderr.is_empty() { stderr } else { stdout };
        Ok(summary)
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git push failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// Raw `git diff --staged` text — everything currently in the index that
/// would land in the next commit. Empty string when nothing is staged
/// (matches git's own zero-output behaviour). Errors carry stderr verbatim
/// so the panel can surface "fatal: not a git repository" etc.
pub fn diff_staged(root: &Path) -> Result<String, String> {
    diff_text(root, &["diff", "--staged"])
}

/// Raw `git diff <branch>` text — working-tree state versus the tip of
/// `branch`. Used by the Source Control dropdown's "View Changes vs
/// <default>" so users can preview every uncommitted-plus-committed change
/// on the current branch in one view.
pub fn diff_against_branch(root: &Path, branch: &str) -> Result<String, String> {
    diff_text(root, &["diff", branch])
}

/// Raw `git diff HEAD~1` text — working-tree state versus the commit
/// before HEAD. Used by the Source Control dropdown's "View Changes vs
/// previous" so users can preview everything that changed since the last
/// commit. Errors (e.g. "fatal: ambiguous argument 'HEAD~1'" on a repo
/// with a single commit) carry stderr verbatim so the panel can surface
/// the reason instead of a silent no-op.
pub fn diff_previous_commit(root: &Path) -> Result<String, String> {
    diff_text(root, &["diff", "HEAD~1"])
}

fn diff_text(root: &Path, args: &[&str]) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str]).args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git {args:?} failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// One commit in a file's history, for the Explorer TIMELINE view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHistoryEntry {
    pub short_hash: String,
    pub summary: String,
    pub author: String,
    /// Seconds elapsed since the commit time, for [`humanize_age`].
    pub age_secs: i64,
}

/// Recent commits touching `rel_path`, newest first, via `git log --follow`
/// (so the history survives renames, like VS Code's Timeline). Returns an
/// empty vec when the path is untracked, the repo has no history, or git is
/// unavailable — the panel renders that as an empty state, never an error.
pub fn file_history(root: &Path, rel_path: &str, limit: usize) -> Vec<FileHistoryEntry> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--follow",
            &format!("-n{limit}"),
            "--format=%h\x1f%s\x1f%an\x1f%ct",
            "--",
            rel_path,
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_file_history(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// Parse the `%h\x1f%s\x1f%an\x1f%ct` lines emitted by [`file_history`] into
/// entries, computing each commit's age relative to `now` (unix seconds).
pub fn parse_file_history(out: &str, now: i64) -> Vec<FileHistoryEntry> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let short_hash = parts.next()?.to_string();
            let summary = parts.next()?.to_string();
            let author = parts.next()?.to_string();
            let ct: i64 = parts.next()?.trim().parse().ok()?;
            if short_hash.is_empty() {
                return None;
            }
            Some(FileHistoryEntry {
                short_hash,
                summary,
                author,
                age_secs: now - ct,
            })
        })
        .collect()
}

/// Raw `git show <hash> -- <rel_path>` text: the file's diff in that commit,
/// for opening a TIMELINE entry in the side-by-side diff viewer.
pub fn show_commit_file_diff(root: &Path, hash: &str, rel_path: &str) -> Result<String, String> {
    diff_text(root, &["show", hash, "--", rel_path])
}

/// The whole commit as text — header, message, diffstat, and full patch —
/// for the Source Control graph's click-to-open view (the tig / lazygit
/// idiom for inspecting a commit in a terminal).
pub fn show_commit(root: &Path, hash: &str) -> Result<String, String> {
    diff_text(root, &["show", "--stat", "--patch", hash])
}

/// One commit of the repo-wide Source Control graph, straight off
/// `git log --topo-order` with its parent hashes (the lane layout's input)
/// and its decorations (branch / tag / HEAD ref names).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphCommit {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    /// Decoration names as git prints them (`HEAD -> main`, `origin/main`,
    /// `tag: v1.0`), split per ref; empty for an undecorated commit.
    pub refs: Vec<String>,
    pub summary: String,
    pub author: String,
    /// Seconds elapsed since the commit time, for [`humanize_age`].
    pub age_secs: i64,
}

/// The newest `limit` commits across local branches, tags, and HEAD in
/// topological order (parents never precede children), the input to the
/// Source Control graph. Empty on any failure — no repo, no commits, no git —
/// so the panel renders an empty state, never an error.
pub fn commit_graph(root: &Path, limit: usize) -> Vec<GraphCommit> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--branches",
            "--tags",
            "HEAD",
            "--topo-order",
            &format!("-n{limit}"),
            "--format=%H\x1f%h\x1f%P\x1f%D\x1f%s\x1f%an\x1f%ct",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_commit_graph(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// Parse the `%H\x1f%h\x1f%P\x1f%D\x1f%s\x1f%an\x1f%ct` lines emitted by
/// [`commit_graph`], computing each commit's age relative to `now`.
pub fn parse_commit_graph(out: &str, now: i64) -> Vec<GraphCommit> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let hash = parts.next()?.to_string();
            let short_hash = parts.next()?.to_string();
            let parents: Vec<String> = parts
                .next()?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let refs: Vec<String> = parts
                .next()?
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let summary = parts.next()?.to_string();
            let author = parts.next()?.to_string();
            let ct: i64 = parts.next()?.trim().parse().ok()?;
            if hash.is_empty() {
                return None;
            }
            Some(GraphCommit {
                hash,
                short_hash,
                parents,
                refs,
                summary,
                author,
                age_secs: (now - ct).max(0),
            })
        })
        .collect()
}

/// The commit that last touched one line, for GitLens-style inline blame.
/// Lines not yet committed (staged or unstaged working-tree edits) blame
/// against the all-zero hash, which [`parse_blame`] marks `uncommitted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub short_hash: String,
    pub summary: String,
    pub author: String,
    /// Seconds since the commit's author-time, for [`humanize_age`].
    pub age_secs: i64,
    /// True for a not-yet-committed line (the zero hash `git blame` reports
    /// for working-tree changes).
    pub uncommitted: bool,
}

/// Per-line blame for `rel_path`, indexed 0-based by result position (result
/// `[0]` is line 1). Uses `git blame --line-porcelain` so each line carries
/// its author, author-time, and summary. Returns empty on any failure so the
/// caller renders no annotation rather than an error.
pub fn blame(root: &Path, rel_path: &str) -> Vec<BlameLine> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", path_str, "blame", "--line-porcelain", "--", rel_path])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_blame(
        &String::from_utf8_lossy(&output.stdout),
        current_unix_seconds(),
    )
}

/// Parse `git blame --line-porcelain` output into one [`BlameLine`] per source
/// line, in order. Porcelain groups each line as a header (`<hash> <orig>
/// <final> [group]`) followed by `author`, `author-time`, `summary`, etc.,
/// then a tab-prefixed content line. `now` (unix seconds) dates each line.
pub fn parse_blame(out: &str, now: i64) -> Vec<BlameLine> {
    let mut lines = Vec::new();
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
        } else if raw.starts_with('\t') {
            // The content line closes a group: emit the accumulated blame.
            if let Some(full) = hash.take() {
                let uncommitted = full.chars().all(|c| c == '0');
                lines.push(BlameLine {
                    short_hash: full.chars().take(8).collect(),
                    summary: std::mem::take(&mut summary),
                    author: std::mem::take(&mut author),
                    age_secs: now - author_time,
                    uncommitted,
                });
            }
        } else if let Some(h) = raw.split(' ').next() {
            // A header line begins with a 40-char hash; other porcelain
            // metadata (previous/filename/committer/…) is ignored.
            if h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()) {
                hash = Some(h.to_string());
            }
        }
    }
    lines
}

/// Repo's default branch name. Tries `origin/HEAD` first (set by
/// `git clone` / `git remote set-head -a origin`), then falls back to
/// common local names (`main`, `master`, `develop`, `trunk`) by checking
/// `git rev-parse --verify <name>`. Returns `Err` when none resolves so
/// the caller can surface the precise failure instead of guessing.
pub fn default_branch(root: &Path) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    if let Ok(out) = Command::new("git")
        .args([
            "-C",
            path_str,
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        && out.status.success()
    {
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(short) = raw.strip_prefix("origin/") {
            if !short.is_empty() {
                return Ok(short.to_string());
            }
        } else if !raw.is_empty() {
            return Ok(raw);
        }
    }
    for candidate in ["main", "master", "develop", "trunk"] {
        let out = Command::new("git")
            .args([
                "-C",
                path_str,
                "rev-parse",
                "--verify",
                "--quiet",
                candidate,
            ])
            .output();
        if let Ok(o) = out
            && o.status.success()
        {
            return Ok(candidate.to_string());
        }
    }
    Err(
        "Could not resolve a default branch (tried origin/HEAD, main, master, develop, trunk)"
            .to_string(),
    )
}

/// Stage a single path. Convenience wrapper over `stage_paths` for the
/// inline "+" icon click path; multi-select staging goes through
/// `stage_paths` directly so all paths land in one atomic `git add`
/// invocation (the index.lock is held only once, so the background
/// status worker can't race us between adds).
pub fn stage_path(root: &Path, rel_path: &str) -> Result<(), String> {
    stage_paths(root, std::slice::from_ref(&rel_path.to_string()))
}

/// Stage all listed paths in a single `git add -- p1 p2 ...` invocation.
/// Atomic with respect to the index lock — running N separate `git add`
/// commands lets the background `git status` worker grab the lock
/// between them and fail later adds with "Unable to create index.lock".
pub fn stage_paths(root: &Path, rel_paths: &[String]) -> Result<(), String> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str, "add", "--"]);
    for p in rel_paths {
        cmd.arg(p);
    }
    let output = cmd
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

/// Unstage a single path. Convenience wrapper over `unstage_paths` for
/// the inline "−" icon click on a staged row.
pub fn unstage_path(root: &Path, rel_path: &str) -> Result<(), String> {
    unstage_paths(root, std::slice::from_ref(&rel_path.to_string()))
}

/// Move all listed paths out of the index and back into the working tree,
/// in one `git reset -q HEAD -- p1 p2 …` invocation. The mirror of
/// `stage_paths`: `git reset HEAD` is what VS Code's git extension runs to
/// unstage, and unlike `git restore --staged` it also unstages files added
/// in a repo's very first commit. Atomic w.r.t. the index lock, so the
/// background status worker can't race us between paths.
pub fn unstage_paths(root: &Path, rel_paths: &[String]) -> Result<(), String> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = vec!["reset", "-q", "HEAD", "--"];
    args.extend(rel_paths.iter().map(String::as_str));
    run_mutation(root, &args).map(|_| ())
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
        let path_str = root
            .to_str()
            .ok_or_else(|| "non-utf8 workspace path".to_string())?;
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

/// Pull the current branch from its upstream (`git pull`). Sibling of
/// `push_current_branch`; surfaced by the commit dropdown's "Pull" item
/// and used by `sync` below. Returns git's verbatim summary ("Already
/// up to date." / the fast-forward range) so the panel can echo it.
pub fn pull_current_branch(root: &Path) -> Result<String, String> {
    run_mutation(root, &["pull"])
}

/// A single branch the Checkout/Create picker can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// What the user sees: the local branch name, or `origin/foo` for a
    /// remote-tracking branch.
    pub display: String,
    /// What to hand `git switch`: the local name, or the short name of a
    /// remote branch (so `git switch foo` DWIMs the tracking branch).
    pub checkout_name: String,
    /// True for the branch currently checked out (marked in the picker,
    /// never offered as a checkout target).
    pub is_current: bool,
    /// True for `origin/…` remote-tracking branches, listed below locals.
    pub is_remote: bool,
}

/// List branches for the Checkout picker: local branches first (most
/// recently committed on top, the current one flagged), then remote-only
/// branches whose name has no local counterpart. Remote rows carry the
/// short `checkout_name` so selecting `origin/foo` runs `git switch foo`
/// and lets git create the tracking branch.
pub fn list_branches(root: &Path) -> Result<Vec<BranchInfo>, String> {
    let locals_raw = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(HEAD)",
            "refs/heads",
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut branches: Vec<BranchInfo> = Vec::new();
    let mut local_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in locals_raw.lines() {
        let mut parts = line.split('\t');
        let Some(name) = parts.next() else { continue };
        if name.is_empty() {
            continue;
        }
        let is_current = parts.next().map(|m| m.trim() == "*").unwrap_or(false);
        local_names.insert(name.to_string());
        branches.push(BranchInfo {
            display: name.to_string(),
            checkout_name: name.to_string(),
            is_current,
            is_remote: false,
        });
    }
    let remotes_raw = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    for full in remotes_raw.lines() {
        // `git symbolic-ref refs/remotes/origin/HEAD` shows up as e.g.
        // "origin/HEAD" — skip those pointer refs, they aren't branches.
        if full.is_empty() || full.ends_with("/HEAD") {
            continue;
        }
        // Strip the leading "<remote>/" to get the short name git switch wants.
        let short = full.split_once('/').map(|(_, s)| s).unwrap_or(full);
        if local_names.contains(short) {
            continue;
        }
        branches.push(BranchInfo {
            display: full.to_string(),
            checkout_name: short.to_string(),
            is_current: false,
            is_remote: true,
        });
    }
    Ok(branches)
}

/// Switch to an existing branch (`git switch <name>`). For a remote-only
/// branch pass its short name so git creates the local tracking branch.
pub fn checkout_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["switch", name])
}

/// Create a new branch off the current HEAD and switch to it
/// (`git switch -c <name>`). Errors (e.g. name already exists) carry
/// git's verbatim message.
pub fn create_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "-c", name])
}

/// Stash the working tree (`git stash push`). Tracked changes are saved
/// and the working tree reverts to HEAD, exactly like VS Code's Stash.
pub fn stash_push(root: &Path) -> Result<String, String> {
    run_mutation(root, &["stash", "push"])
}

/// Restore and drop the most recent stash (`git stash pop`). A pop
/// conflict surfaces git's verbatim message so the user can resolve it.
pub fn stash_pop(root: &Path) -> Result<String, String> {
    run_mutation(root, &["stash", "pop"])
}

// --- Fetch / Clone -------------------------------------------------------

/// Fetch from all remotes without merging (`git fetch --all --prune`).
/// VS Code's "Fetch" updates remote-tracking refs so ahead/behind counts
/// refresh; `--prune` drops refs deleted on the remote.
pub fn fetch_all(root: &Path) -> Result<String, String> {
    run_mutation(root, &["fetch", "--all", "--prune"])
}

/// Derive the directory name a clone of `url` would land in: the final
/// path segment with any trailing `.git` and slash stripped. Returns
/// `None` when the URL has no usable segment.
pub fn clone_dir_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let seg = trimmed.rsplit(['/', ':']).next()?.trim();
    let name = seg.strip_suffix(".git").unwrap_or(seg).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Clone `url` into `parent/<repo-name>` and return the new repo path.
/// The Source Control "Clone" flow prompts for the URL, clones beside the
/// current workspace, then re-roots croft into the clone.
pub fn clone_into(parent: &Path, url: &str) -> Result<PathBuf, String> {
    let name =
        clone_dir_name(url).ok_or_else(|| format!("can't derive a folder name from {url}"))?;
    let dest = parent.join(&name);
    // run_mutation runs with `-C <parent>`, so the bare `<name>` clones into
    // parent/<name>.
    run_mutation(parent, &["clone", url, &name]).map(|_| dest)
}

// --- Commit variants -----------------------------------------------------

/// Commit only what is already staged (`git commit -m`), leaving unstaged
/// changes in the working tree. The mirror of `commit_all_tracked`, which
/// auto-stages tracked edits with `-am`.
pub fn commit_staged(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    run_mutation(root, &["commit", "-m", message])
}

/// Amend the previous commit with a new message (`git commit --amend -m`),
/// folding any currently-staged changes into it. Rewrites history, so the
/// caller confirms before invoking.
pub fn commit_amend(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    run_mutation(root, &["commit", "--amend", "-m", message])
}

/// Amend the previous commit keeping its existing message
/// (`git commit --amend --no-edit`); folds staged changes into HEAD.
pub fn commit_amend_no_edit(root: &Path) -> Result<String, String> {
    run_mutation(root, &["commit", "--amend", "--no-edit"])
}

// --- Bulk staging --------------------------------------------------------

/// Stage every change, tracked and untracked (`git add -A`).
pub fn stage_all(root: &Path) -> Result<String, String> {
    run_mutation(root, &["add", "-A"])
}

/// Unstage everything (`git reset -q HEAD`), moving the whole index back
/// into the working tree.
pub fn unstage_all(root: &Path) -> Result<String, String> {
    run_mutation(root, &["reset", "-q", "HEAD"])
}

/// Discard every tracked modification (`git checkout -- .`) — destructive,
/// the caller MUST confirm. Untracked files are left untouched (matching
/// the per-file discard, which deletes untracked only on explicit request).
pub fn discard_all_tracked(root: &Path) -> Result<String, String> {
    run_mutation(root, &["checkout", "--", "."])
}

// --- Pull / Push variants ------------------------------------------------

/// Pull with rebase instead of merge (`git pull --rebase`).
pub fn pull_rebase(root: &Path) -> Result<String, String> {
    run_mutation(root, &["pull", "--rebase"])
}

/// Force-push the current branch, but only if the remote hasn't advanced
/// past what we last saw (`git push --force-with-lease`). Safer than a
/// bare `--force`; VS Code's "Push (Force)" uses the same lease guard.
pub fn push_force(root: &Path) -> Result<String, String> {
    run_mutation(root, &["push", "--force-with-lease"])
}

/// Push the current branch to a specific remote (`git push <remote>`).
pub fn push_to_remote(root: &Path, remote: &str) -> Result<String, String> {
    run_mutation(root, &["push", remote])
}

/// Publish the current branch: push it to `origin` and set upstream
/// tracking (`git push -u origin <branch>`). Used when a local branch has
/// no upstream yet.
pub fn publish_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation(root, &["push", "-u", "origin", branch])
}

// --- Branch management ---------------------------------------------------

/// Create a new branch off an explicit base ref and switch to it
/// (`git switch -c <name> <base>`). Powers "Create Branch from…".
pub fn create_branch_from(root: &Path, name: &str, base: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "-c", name, base])
}

/// Rename the current branch (`git branch -m <new>`).
pub fn rename_branch(root: &Path, new_name: &str) -> Result<String, String> {
    run_mutation(root, &["branch", "-m", new_name])
}

/// Delete a branch (`git branch -d <name>`). Uses the safe `-d`, which
/// refuses to drop unmerged work; the error is surfaced verbatim so the
/// user can decide whether to force.
pub fn delete_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["branch", "-d", name])
}

/// Merge `branch` into the current branch (`git merge <branch>`).
pub fn merge_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation(root, &["merge", branch])
}

/// Rebase the current branch onto `branch` (`git rebase <branch>`).
pub fn rebase_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation(root, &["rebase", branch])
}

// --- Remotes -------------------------------------------------------------

/// A configured remote: its name and fetch URL, for the Remote submenu's
/// pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// List configured remotes (`git remote -v`), de-duplicated to one row per
/// remote (the fetch URL).
pub fn list_remotes(root: &Path) -> Result<Vec<RemoteInfo>, String> {
    let raw = run_git(root, &["remote", "-v"]).map_err(|e| e.to_string())?;
    let mut out: Vec<RemoteInfo> = Vec::new();
    for line in raw.lines() {
        // Format: "<name>\t<url> (fetch|push)"
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
            continue;
        };
        if out.iter().any(|r| r.name == name) {
            continue;
        }
        out.push(RemoteInfo {
            name: name.to_string(),
            url: url.to_string(),
        });
    }
    Ok(out)
}

/// Add a remote (`git remote add <name> <url>`).
pub fn add_remote(root: &Path, name: &str, url: &str) -> Result<String, String> {
    run_mutation(root, &["remote", "add", name, url])
}

/// Remove a remote (`git remote remove <name>`).
pub fn remove_remote(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["remote", "remove", name])
}

// --- Stash (full set) ----------------------------------------------------

/// Stash including untracked files (`git stash push -u`).
pub fn stash_push_untracked(root: &Path) -> Result<String, String> {
    run_mutation(root, &["stash", "push", "-u"])
}

/// Stash only the staged changes (`git stash push --staged`).
pub fn stash_push_staged(root: &Path) -> Result<String, String> {
    run_mutation(root, &["stash", "push", "--staged"])
}

/// One entry in the stash list, for the apply/pop/drop pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashInfo {
    /// Stash index (`stash@{N}` is built from this).
    pub index: usize,
    /// The human description git stores (`WIP on main: …`).
    pub message: String,
}

/// List stashes newest-first (`git stash list`).
pub fn list_stashes(root: &Path) -> Result<Vec<StashInfo>, String> {
    let raw =
        run_git(root, &["stash", "list", "--format=%gd\x1f%gs"]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut parts = line.split('\x1f');
        let Some(reflog) = parts.next() else { continue };
        let message = parts.next().unwrap_or("").to_string();
        // reflog looks like "stash@{2}" — pull the integer out.
        let index = reflog
            .trim_start_matches("stash@{")
            .trim_end_matches('}')
            .parse::<usize>()
            .unwrap_or(out.len());
        out.push(StashInfo { index, message });
    }
    Ok(out)
}

/// Apply a stash without dropping it (`git stash apply stash@{N}`).
pub fn stash_apply(root: &Path, index: usize) -> Result<String, String> {
    run_mutation(root, &["stash", "apply", &format!("stash@{{{index}}}")])
}

/// Apply and drop a specific stash (`git stash pop stash@{N}`).
pub fn stash_pop_at(root: &Path, index: usize) -> Result<String, String> {
    run_mutation(root, &["stash", "pop", &format!("stash@{{{index}}}")])
}

/// Drop a stash without applying it (`git stash drop stash@{N}`).
pub fn stash_drop(root: &Path, index: usize) -> Result<String, String> {
    run_mutation(root, &["stash", "drop", &format!("stash@{{{index}}}")])
}

// --- Tags ----------------------------------------------------------------

/// List tags newest-first (`git tag --sort=-creatordate`).
pub fn list_tags(root: &Path) -> Result<Vec<String>, String> {
    let raw = run_git(root, &["tag", "--sort=-creatordate"]).map_err(|e| e.to_string())?;
    Ok(raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Create a lightweight tag at HEAD (`git tag <name>`).
pub fn create_tag(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["tag", name])
}

/// Delete a tag (`git tag -d <name>`).
pub fn delete_tag(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["tag", "-d", name])
}

/// Render an elapsed-seconds count as a coarse relative time (e.g.
/// "2 hours ago"), used by the Explorer TIMELINE view. Negative inputs
/// clamp to zero.
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
    StatusAndChanges,
    SetRoot(PathBuf),
}

impl GitRequest {
    /// Merge two pending *query* requests into the strongest one.
    /// `StatusAndChanges` dominates a bare `Status`; same+same is the same.
    /// `SetRoot` is never a query and the worker drains it inline before
    /// merging, so it must not appear here.
    pub fn merge(self, other: Self) -> Self {
        use GitRequest::*;
        match (self, other) {
            (SetRoot(_), _) | (_, SetRoot(_)) => {
                unreachable!(
                    "SetRoot is drained inline before merge; it cannot reach the query merge"
                )
            }
            (StatusAndChanges, _) | (_, StatusAndChanges) => StatusAndChanges,
            (Status, Status) => Status,
        }
    }
}

/// Result the worker ships back. The variants mirror `GitRequest` 1:1
/// so the App can route each response to the right consumer without an
/// extra request-id channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitResponse {
    Status(GitStatus),
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
    fn parse_commit_graph_splits_parents_and_refs() {
        let line = format!(
            "{h}\x1f{s}\x1f{p1} {p2}\x1fHEAD -> main, tag: v1.0, origin/main\x1ffeat: merge\x1fvitali87\x1f1000",
            h = "a".repeat(40),
            s = "aaaaaaa",
            p1 = "b".repeat(40),
            p2 = "c".repeat(40),
        );
        let commits = parse_commit_graph(&line, 4600);
        assert_eq!(commits.len(), 1);
        let c = &commits[0];
        assert_eq!(c.short_hash, "aaaaaaa");
        assert_eq!(c.parents, vec!["b".repeat(40), "c".repeat(40)]);
        assert_eq!(
            c.refs,
            vec![
                "HEAD -> main".to_string(),
                "tag: v1.0".to_string(),
                "origin/main".to_string()
            ]
        );
        assert_eq!(c.summary, "feat: merge");
        assert_eq!(c.age_secs, 3600);
        // A root commit has an empty %P field → no parents.
        let root_line = format!(
            "{h}\x1fddddddd\x1f\x1f\x1finit\x1ft\x1f1",
            h = "d".repeat(40)
        );
        let commits = parse_commit_graph(&root_line, 1);
        assert!(commits[0].parents.is_empty());
        assert!(commits[0].refs.is_empty());
    }

    /// End to end against a REAL repo: init, commit, branch, merge — then
    /// assert `commit_graph` + the lane layout produce the diamond rails.
    #[test]
    fn commit_graph_of_a_real_merge_lays_out_the_diamond() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-C", root.to_str().unwrap()])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "1\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "root"]);
        run(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("b.txt"), "2\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "feature work"]);
        run(&["checkout", "-q", "main"]);
        std::fs::write(root.join("c.txt"), "3\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "main work"]);
        run(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);

        let commits = commit_graph(&root, 50);
        assert_eq!(commits.len(), 4, "root, two branches, one merge");
        assert_eq!(commits[0].summary, "merge feature");
        assert_eq!(commits[0].parents.len(), 2, "the merge has two parents");
        assert!(
            commits[0].refs.iter().any(|r| r.contains("main")),
            "HEAD decoration must survive parsing; got {:?}",
            commits[0].refs
        );
        let rows = crate::widgets::commit_graph::layout_graph(commits);
        let rails: Vec<String> = rows
            .iter()
            .map(|r| r.cells.iter().map(|(ch, _)| *ch).collect())
            .collect();
        // git's --topo-order emits the second parent's subtree (feature work,
        // lane 1) before the first parent's (main work, lane 0) here; either
        // interleaving is a valid diamond, the shape is what matters.
        assert_eq!(
            rails,
            vec!["●╮", "│●", "●│", "●╯"],
            "a no-ff merge of one branch must draw the diamond"
        );
    }

    /// Init a repo, commit a 20-line file, then edit line 2 and line 18
    /// so the working tree carries two well-separated hunks.
    fn repo_with_two_hunks(root: &Path) {
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-C", root.to_str().unwrap()])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        let original: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("f.txt"), original).unwrap();
        run(&["add", "f.txt"]);
        run(&["commit", "-q", "-m", "init"]);
        let mut edited: Vec<String> = (1..=20).map(|i| format!("line{i}")).collect();
        edited[1] = "LINE2".into();
        edited[17] = "LINE18".into();
        std::fs::write(root.join("f.txt"), edited.join("\n") + "\n").unwrap();
    }

    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-C", root.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    #[test]
    fn apply_patch_cached_stages_only_the_given_hunk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        repo_with_two_hunks(root);
        let head = read_file_at_head(root, "f.txt").unwrap();
        let work = std::fs::read_to_string(root.join("f.txt")).unwrap();
        let diff = crate::widgets::diff::DiffData::build(
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            head.lines().map(str::to_string).collect(),
            work.lines().map(str::to_string).collect(),
        );
        let range = diff.hunk_range_at(0).unwrap();
        let patch = diff.hunk_patch("f.txt", range);
        apply_patch(root, &patch, true, false).unwrap();
        let staged = git_stdout(root, &["diff", "--cached"]);
        assert!(staged.contains("+LINE2"), "hunk 1 must be staged: {staged}");
        assert!(
            !staged.contains("LINE18"),
            "hunk 2 must NOT be staged: {staged}"
        );
        let unstaged = git_stdout(root, &["diff"]);
        assert!(
            unstaged.contains("+LINE18"),
            "hunk 2 must stay in the working tree: {unstaged}"
        );
        // Round-trip: unstage the same hunk again.
        apply_patch(root, &patch, true, true).unwrap();
        let staged = git_stdout(root, &["diff", "--cached"]);
        assert!(
            staged.trim().is_empty(),
            "reverse cached apply must empty the index diff: {staged}"
        );
    }

    #[test]
    fn apply_patch_reverse_reverts_only_the_given_hunk_in_the_working_tree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        repo_with_two_hunks(root);
        let head = read_file_at_head(root, "f.txt").unwrap();
        let work = std::fs::read_to_string(root.join("f.txt")).unwrap();
        let diff = crate::widgets::diff::DiffData::build(
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            head.lines().map(str::to_string).collect(),
            work.lines().map(str::to_string).collect(),
        );
        let range = diff.hunk_range_at(0).unwrap();
        let patch = diff.hunk_patch("f.txt", range);
        apply_patch(root, &patch, false, true).unwrap();
        let after = std::fs::read_to_string(root.join("f.txt")).unwrap();
        assert!(after.contains("line2\n"), "hunk 1 must be reverted");
        assert!(!after.contains("LINE2"), "hunk 1 edit must be gone");
        assert!(after.contains("LINE18"), "hunk 2 must survive the revert");
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
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
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
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        // Make a working-tree change.
        std::fs::write(p.join("hello.txt"), "hi").unwrap();
        let s = query(p);
        assert!(s.in_repo);
        assert!(s.dirty);
    }

    #[test]
    fn query_collects_ignored_files_and_collapsed_dirs() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        std::fs::write(p.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        std::fs::write(p.join("app.log"), "").unwrap();
        std::fs::write(p.join("keep.txt"), "").unwrap();
        std::fs::create_dir(p.join("build")).unwrap();
        std::fs::write(p.join("build/out.txt"), "").unwrap();
        let s = query(p);
        assert!(s.ignored.contains(&p.join("app.log")));
        assert!(
            s.ignored.contains(&p.join("build")),
            "a fully-ignored directory collapses to one entry, not its contents"
        );
        assert!(
            !s.ignored.contains(&p.join("build/out.txt")),
            "children of a collapsed ignored dir are not enumerated"
        );
        assert!(!s.ignored.contains(&p.join("keep.txt")));
        assert!(!s.ignored.contains(&p.join(".gitignore")));
    }

    #[test]
    fn a_directory_git_does_not_ignore_is_not_reported_as_ignored() {
        // `ls-files --directory` collapses any ENTIRELY untracked directory
        // whose contents all happen to be ignored, even when no rule matches
        // the directory itself. Reporting it would grey a folder VS Code
        // paints normally (it asks check-ignore per resource).
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        std::fs::write(p.join(".gitignore"), "*.log\n").unwrap();
        std::fs::create_dir(p.join("logs")).unwrap();
        std::fs::write(p.join("logs/app.log"), "").unwrap();
        let s = query(p);
        assert!(
            s.ignored.contains(&p.join("logs/app.log")),
            "the ignored file itself is still reported"
        );
        assert!(
            !s.ignored.contains(&p.join("logs")),
            "no rule matches `logs` itself, so it must not be greyed"
        );
    }

    #[test]
    fn a_filename_with_a_leading_space_is_reported_intact() {
        // git sorts its output, so a leading-space name comes first — and a
        // blanket trim() on the NUL-separated blob eats that space, naming a
        // file that does not exist while the real one stays unmarked.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        std::fs::write(p.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(p.join(" leading.log"), "").unwrap();
        std::fs::write(p.join("zz.log"), "").unwrap();
        let s = query(p);
        assert!(
            s.ignored.contains(&p.join(" leading.log")),
            "the leading space belongs to the filename; got {:?}",
            s.ignored
        );
        assert!(s.ignored.contains(&p.join("zz.log")));
    }

    #[test]
    fn query_outside_a_repo_has_an_empty_ignored_set() {
        let tmp = TempDir::new().unwrap();
        let s = query(tmp.path());
        assert!(s.ignored.is_empty());
    }

    #[test]
    fn diff_previous_commit_shows_the_delta_since_the_last_commit() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(p)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("f.txt"), "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(p.join("f.txt"), "two\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "second"]);
        // Working tree matches HEAD (the second commit), so `git diff HEAD~1`
        // is exactly the first->second delta.
        let raw = diff_previous_commit(p).unwrap();
        assert!(
            raw.contains("-one"),
            "diff vs previous commit must show the removed line; got: {raw}"
        );
        assert!(
            raw.contains("+two"),
            "diff vs previous commit must show the added line; got: {raw}"
        );
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
    fn parse_porcelain_changes_handles_unstaged_modified() {
        let entries = parse_porcelain_changes(" M src/app.rs\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "src/app.rs".to_string(),
                kind: ChangeKind::Modified,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn parse_porcelain_changes_handles_staged_modified() {
        let entries = parse_porcelain_changes("M  src/app.rs\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "src/app.rs".to_string(),
                kind: ChangeKind::StagedModified,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn parse_porcelain_changes_emits_two_entries_for_partially_staged() {
        // MM: staged content then further modified in worktree.
        let entries = parse_porcelain_changes("MM src/app.rs\n");
        assert_eq!(
            entries,
            vec![
                ChangeEntry {
                    path: "src/app.rs".to_string(),
                    kind: ChangeKind::StagedModified,
                    ..Default::default()
                },
                ChangeEntry {
                    path: "src/app.rs".to_string(),
                    kind: ChangeKind::Modified,
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn parse_porcelain_changes_handles_untracked() {
        let entries = parse_porcelain_changes("?? scratch.txt\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "scratch.txt".to_string(),
                kind: ChangeKind::Untracked,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn parse_porcelain_changes_handles_rename_takes_destination_path() {
        let entries = parse_porcelain_changes("R  old.txt -> new.txt\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "new.txt".to_string(),
                kind: ChangeKind::StagedRenamed,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn unquote_porcelain_path_passes_plain_ascii_paths_through() {
        assert_eq!(unquote_porcelain_path("README.md"), "README.md");
        assert_eq!(unquote_porcelain_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn unquote_porcelain_path_strips_double_quotes_and_unescapes_spaces() {
        // `git status --porcelain` wraps filenames containing spaces in
        // double quotes. The user's repro: a Finder "Duplicate" producing
        // `linked_list copy.py`, which arrives as `"linked_list copy.py"`.
        assert_eq!(
            unquote_porcelain_path("\"linked_list copy.py\""),
            "linked_list copy.py",
        );
    }

    #[test]
    fn unquote_porcelain_path_decodes_c_style_escapes() {
        assert_eq!(
            unquote_porcelain_path(r#""line\nbreak.txt""#),
            "line\nbreak.txt"
        );
        assert_eq!(
            unquote_porcelain_path(r#""quote\"inside.txt""#),
            "quote\"inside.txt"
        );
        assert_eq!(
            unquote_porcelain_path(r#""back\\slash.txt""#),
            "back\\slash.txt"
        );
    }

    #[test]
    fn unquote_porcelain_path_decodes_octal_utf8_sequences() {
        // `core.quotepath = true` (default) escapes non-ASCII bytes as
        // 3-digit octal. `ä` is UTF-8 0xC3 0xA4 → `\303\244`.
        assert_eq!(unquote_porcelain_path(r#""f\303\244.txt""#), "fä.txt");
    }

    #[test]
    fn parse_porcelain_changes_de_quotes_paths_with_spaces() {
        // Regression for the user's "Stage failed: fatal: pathspec
        // '"linked_list copy.py"' did not match any files" report.
        // The parser must strip the surrounding double quotes so the
        // downstream `git add -- <path>` call hits the real filename.
        let entries = parse_porcelain_changes("?? \"linked_list copy.py\"\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "linked_list copy.py".to_string(),
                kind: ChangeKind::Untracked,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn parse_porcelain_changes_handles_conflict() {
        let entries = parse_porcelain_changes("UU merge.txt\n");
        assert_eq!(
            entries,
            vec![ChangeEntry {
                path: "merge.txt".to_string(),
                kind: ChangeKind::Conflicted,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn parse_numstat_extracts_additions_deletions_per_path() {
        let out = "12\t3\tsrc/app.rs\n5\t0\tREADME.md\n-\t-\tassets/logo.png\n";
        let map = parse_numstat(out);
        assert_eq!(map.get("src/app.rs"), Some(&(12, 3)));
        assert_eq!(map.get("README.md"), Some(&(5, 0)));
        assert_eq!(map.get("assets/logo.png"), Some(&(0, 0)));
    }

    #[test]
    fn parse_numstat_keys_renames_on_destination_path() {
        let out = "2\t1\told.rs -> new.rs\n";
        let map = parse_numstat(out);
        assert_eq!(map.get("new.rs"), Some(&(2, 1)));
        assert!(!map.contains_key("old.rs"));
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
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.name", "a"])
            .status();
        std::fs::write(p.join("hello.txt"), "hi").unwrap();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["add", "."])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["commit", "-m", "init", "--quiet"])
            .status();
        std::fs::write(p.join("hello.txt"), "hi v2").unwrap();
        let summary = commit_all_tracked(p, "second commit").expect("commit should succeed");
        assert!(
            summary.contains("second commit") || summary.contains("main"),
            "summary was: {summary}"
        );
        assert!(
            query_changes(p).is_empty(),
            "post-commit working tree should be clean"
        );
    }

    #[test]
    fn git_request_merge_collapses_status_and_changes_into_status_and_changes() {
        assert_eq!(
            GitRequest::Status.merge(GitRequest::Status),
            GitRequest::Status,
        );
        assert_eq!(
            GitRequest::Status.merge(GitRequest::StatusAndChanges),
            GitRequest::StatusAndChanges,
        );
        assert_eq!(
            GitRequest::StatusAndChanges.merge(GitRequest::Status),
            GitRequest::StatusAndChanges,
        );
    }

    #[test]
    fn git_worker_loop_processes_a_status_and_changes_request_and_returns_entries() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.name", "a"])
            .status();
        std::fs::write(p.join("untracked.txt"), "x").unwrap();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<GitResponse>();
        let root = p.to_path_buf();
        let join = std::thread::spawn(move || git_worker_loop(root, req_rx, resp_tx));
        req_tx.send(GitRequest::StatusAndChanges).unwrap();
        let resp = resp_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker must reply within 10s");
        match resp {
            GitResponse::StatusAndChanges(_, entries) => {
                assert_eq!(
                    entries.len(),
                    1,
                    "expected 1 untracked entry, got {entries:?}"
                );
                assert_eq!(entries[0].kind, ChangeKind::Untracked);
            }
            other => panic!("expected StatusAndChanges, got {other:?}"),
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
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.name", "a"])
            .status();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<GitRequest>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<GitResponse>();
        let root = p.to_path_buf();
        // Queue two requests BEFORE the worker starts so the first
        // recv() succeeds and the try_recv coalesce sees the second one.
        req_tx.send(GitRequest::Status).unwrap();
        req_tx.send(GitRequest::StatusAndChanges).unwrap();
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

    fn init_repo_with_commit(p: &Path) {
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.email", "a@b"])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["config", "user.name", "a"])
            .status();
        std::fs::write(p.join("seed.txt"), "one\ntwo\n").unwrap();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["add", "."])
            .status();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["commit", "-m", "init", "--quiet"])
            .status();
    }

    #[test]
    fn diff_staged_returns_only_indexed_changes() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["add", "seed.txt"])
            .status();
        std::fs::write(p.join("untracked.txt"), "z").unwrap();
        let out = diff_staged(p).expect("git diff --staged should succeed");
        assert!(
            out.contains("+three"),
            "staged diff must include the +three line that was indexed: {out}"
        );
        assert!(
            !out.contains("untracked.txt"),
            "untracked file must NOT appear in staged diff: {out}"
        );
    }

    #[test]
    fn default_branch_resolves_main_when_only_local() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let b = default_branch(p).expect("default branch must resolve");
        assert_eq!(b, "main", "freshly init -b main repo must resolve to main");
    }

    #[test]
    fn diff_against_branch_shows_working_tree_versus_branch() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
        let out = diff_against_branch(p, "main").expect("diff main should succeed");
        assert!(
            out.contains("+three"),
            "diff vs branch must include the new +three line: {out}"
        );
    }

    #[test]
    fn parse_file_history_splits_fields_and_computes_age() {
        // Two commits, newest first, in the %h\x1f%s\x1f%an\x1f%ct format.
        let now = 10_000i64;
        let out = "abc123\x1ffix: thing\x1fvitali87\x1f9_996\n\
                   def456\x1ffeat: thing\x1falice\x1f7600"
            .replace('_', "");
        let entries = parse_file_history(&out, now);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].short_hash, "abc123");
        assert_eq!(entries[0].summary, "fix: thing");
        assert_eq!(entries[0].author, "vitali87");
        assert_eq!(
            entries[0].age_secs, 4,
            "now (10000) minus commit time (9996)"
        );
        assert_eq!(entries[1].age_secs, 2400);
        // A summary containing the field separator is impossible (git emits the
        // literal subject), but a malformed line missing fields is dropped.
        assert!(parse_file_history("oops-no-fields", now).is_empty());
    }

    #[test]
    fn file_history_returns_real_commits_for_a_tracked_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        // init_repo_with_commit commits seed.txt; its history has >=1 entry.
        let hist = file_history(p, "seed.txt", 10);
        assert!(
            !hist.is_empty(),
            "a committed file must have at least one history entry"
        );
    }

    #[test]
    fn parse_blame_groups_porcelain_into_one_entry_per_line() {
        let now = 10_000i64;
        // Two source lines: line 1 committed, line 2 an uncommitted edit
        // (the all-zero hash git reports for working-tree changes).
        let out = "\
abcdef0123456789abcdef0123456789abcdef01 1 1 1
author Vitali
author-time 9996
summary fix: the thing
filename seed.txt
\tone
0000000000000000000000000000000000000000 2 2 1
author Not Committed Yet
author-time 9999
summary Version of seed.txt from seed.txt
filename seed.txt
\ttwo
";
        let b = parse_blame(out, now);
        assert_eq!(b.len(), 2, "one entry per source line");
        assert_eq!(b[0].short_hash, "abcdef01");
        assert_eq!(b[0].author, "Vitali");
        assert_eq!(b[0].summary, "fix: the thing");
        assert_eq!(b[0].age_secs, 4, "now (10000) minus author-time (9996)");
        assert!(!b[0].uncommitted);
        assert!(b[1].uncommitted, "the zero hash marks an uncommitted line");
    }

    #[test]
    fn blame_returns_one_entry_per_committed_line() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        // seed.txt is "one\ntwo\n" — two committed lines.
        let b = blame(p, "seed.txt");
        assert_eq!(b.len(), 2, "blame reports every source line");
        assert_eq!(
            b[0].author, "a",
            "committed by init_repo_with_commit's user"
        );
        assert!(!b[0].uncommitted);
        assert!(!b[0].short_hash.is_empty());
    }

    /// Stage a path so we can exercise `unstage_*` against a real index.
    fn stage(p: &Path, rel: &str) {
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["add", rel])
            .status();
    }

    /// True when `rel` shows up as a staged entry in `git status --porcelain`
    /// (index column non-space, non-`?`).
    fn is_staged(p: &Path, rel: &str) -> bool {
        query_changes(p)
            .iter()
            .any(|e| e.path == rel && e.kind.section() == ChangeSection::Staged)
    }

    #[test]
    fn unstage_path_moves_a_staged_file_back_to_changes() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
        stage(p, "seed.txt");
        assert!(is_staged(p, "seed.txt"), "precondition: seed.txt is staged");
        unstage_path(p, "seed.txt").expect("unstage must succeed");
        assert!(
            !is_staged(p, "seed.txt"),
            "after unstage the modification is back in the working tree, not the index"
        );
    }

    #[test]
    fn unstage_paths_is_a_noop_on_an_empty_list() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        assert!(unstage_paths(p, &[]).is_ok());
    }

    #[test]
    fn create_branch_then_list_branches_marks_the_new_current_branch() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        create_branch(p, "feature/x").expect("create_branch must succeed");
        let branches = list_branches(p).expect("list_branches must succeed");
        let current: Vec<&BranchInfo> = branches.iter().filter(|b| b.is_current).collect();
        assert_eq!(current.len(), 1, "exactly one branch is current");
        assert_eq!(current[0].display, "feature/x");
        // The original branch is still listed, just not current.
        assert!(
            branches
                .iter()
                .any(|b| b.display == "main" && !b.is_current)
        );
    }

    #[test]
    fn checkout_branch_switches_back_to_an_existing_branch() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        create_branch(p, "feature/x").unwrap();
        checkout_branch(p, "main").expect("checkout_branch must succeed");
        let branch = query(p).branch;
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn stash_push_then_pop_round_trips_a_working_tree_change() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nstashed\n").unwrap();
        stash_push(p).expect("stash push must succeed");
        // The dirty edit is gone from the working tree after stashing.
        assert_eq!(
            std::fs::read_to_string(p.join("seed.txt")).unwrap(),
            "one\ntwo\n"
        );
        stash_pop(p).expect("stash pop must succeed");
        assert_eq!(
            std::fs::read_to_string(p.join("seed.txt")).unwrap(),
            "one\ntwo\nstashed\n",
            "pop restores the stashed edit"
        );
    }

    #[test]
    fn clone_dir_name_strips_dot_git_and_path() {
        assert_eq!(
            clone_dir_name("https://codeberg.org/vitali87/croft.git").as_deref(),
            Some("croft")
        );
        assert_eq!(
            clone_dir_name("git@github.com:owner/repo.git").as_deref(),
            Some("repo")
        );
        assert_eq!(
            clone_dir_name("https://host/path/thing/").as_deref(),
            Some("thing")
        );
        assert_eq!(clone_dir_name("   ").as_deref(), None);
    }

    #[test]
    fn stage_all_then_unstage_all_round_trips_the_index() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(p.join("new.txt"), "x").unwrap();
        stage_all(p).expect("stage_all");
        assert!(is_staged(p, "seed.txt"));
        assert!(is_staged(p, "new.txt"), "untracked file is staged by -A");
        unstage_all(p).expect("unstage_all");
        assert!(!is_staged(p, "seed.txt"));
        assert!(!is_staged(p, "new.txt"));
    }

    #[test]
    fn discard_all_tracked_reverts_modifications_but_keeps_untracked() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nDIRTY\n").unwrap();
        std::fs::write(p.join("scratch.txt"), "keep me").unwrap();
        discard_all_tracked(p).expect("discard_all_tracked");
        assert_eq!(
            std::fs::read_to_string(p.join("seed.txt")).unwrap(),
            "one\ntwo\n",
            "tracked modification is reverted to HEAD"
        );
        assert!(
            p.join("scratch.txt").exists(),
            "untracked files survive a tracked-only discard"
        );
    }

    #[test]
    fn commit_staged_commits_only_the_index() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nstaged\n").unwrap();
        std::fs::write(p.join("other.txt"), "unstaged").unwrap();
        stage(p, "seed.txt");
        commit_staged(p, "only staged").expect("commit_staged");
        // other.txt is still untracked/uncommitted after the staged commit.
        assert!(
            query_changes(p).iter().any(|e| e.path == "other.txt"),
            "an unstaged file must remain a pending change after commit_staged"
        );
    }

    #[test]
    fn rename_branch_changes_the_current_branch_name() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        rename_branch(p, "renamed").expect("rename_branch");
        assert_eq!(query(p).branch.as_deref(), Some("renamed"));
    }

    #[test]
    fn delete_branch_removes_a_merged_branch() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        create_branch(p, "throwaway").unwrap();
        checkout_branch(p, "main").unwrap();
        // throwaway points at the same commit as main, so -d is safe.
        delete_branch(p, "throwaway").expect("delete_branch");
        assert!(
            !list_branches(p)
                .unwrap()
                .iter()
                .any(|b| b.display == "throwaway"),
            "the deleted branch must be gone"
        );
    }

    #[test]
    fn add_then_list_then_remove_remote_round_trips() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        add_remote(p, "upstream", "https://example.com/x.git").expect("add_remote");
        let remotes = list_remotes(p).unwrap();
        assert!(
            remotes
                .iter()
                .any(|r| r.name == "upstream" && r.url == "https://example.com/x.git")
        );
        remove_remote(p, "upstream").expect("remove_remote");
        assert!(
            !list_remotes(p)
                .unwrap()
                .iter()
                .any(|r| r.name == "upstream")
        );
    }

    #[test]
    fn create_then_list_then_delete_tag_round_trips() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        create_tag(p, "v1.0.0").expect("create_tag");
        assert!(list_tags(p).unwrap().contains(&"v1.0.0".to_string()));
        delete_tag(p, "v1.0.0").expect("delete_tag");
        assert!(!list_tags(p).unwrap().contains(&"v1.0.0".to_string()));
    }

    #[test]
    fn stash_list_indexes_newest_first_and_apply_drop_work() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "one\ntwo\nfirst\n").unwrap();
        stash_push(p).expect("first stash");
        std::fs::write(p.join("seed.txt"), "one\ntwo\nsecond\n").unwrap();
        stash_push(p).expect("second stash");
        let stashes = list_stashes(p).unwrap();
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[0].index, 0, "newest stash is stash@{{0}}");
        // Apply the older stash (index 1) without dropping it.
        stash_apply(p, 1).expect("stash_apply");
        assert_eq!(
            std::fs::read_to_string(p.join("seed.txt")).unwrap(),
            "one\ntwo\nfirst\n"
        );
        // Both stashes still present after apply.
        assert_eq!(list_stashes(p).unwrap().len(), 2);
        // Drop the newest; one remains.
        stash_drop(p, 0).expect("stash_drop");
        assert_eq!(list_stashes(p).unwrap().len(), 1);
    }

    #[test]
    fn pull_on_an_up_to_date_clone_succeeds() {
        let upstream = TempDir::new().unwrap();
        let up = upstream.path();
        init_repo_with_commit(up);
        let clone = TempDir::new().unwrap();
        let clone_path = clone.path().join("work");
        let ok = Command::new("git")
            .args(["clone", "-q"])
            .arg(up)
            .arg(&clone_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "clone of the local upstream must succeed");
        // Nothing new upstream, so pull is a no-op but must return Ok with
        // git's "up to date" summary rather than erroring.
        let summary = pull_current_branch(&clone_path).expect("pull must succeed");
        assert!(
            summary.to_lowercase().contains("up to date") || summary.is_empty(),
            "an up-to-date pull reports no new work (got: {summary:?})"
        );
    }
}
