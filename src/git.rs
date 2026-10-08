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
    /// The repository toplevel containing the workspace root, or `None`
    /// outside a repo. Porcelain and numstat paths are TOPLEVEL-relative,
    /// so every consumer that joins a repo-relative path or names one on a
    /// git command line must resolve against this, never the workspace
    /// root — the two coincide only when the workspace root IS the
    /// toplevel (#139).
    pub repo_root: Option<PathBuf>,
    /// Number of changed entries `git status --porcelain` reported —
    /// available for EVERY workspace folder's worker, unlike the panel's
    /// entry list which only the active repo fetches (#161: the
    /// repositories overview and the cross-repo badge read this).
    pub changed_count: usize,
    /// The message git prepared for the commit in progress (#1282): a
    /// conflicted merge, cherry-pick or revert writes `MERGE_MSG`, a
    /// `merge --squash` writes `SQUASH_MSG`. Comment lines are dropped, as
    /// `commit.cleanup=strip` would. `None` when neither file is there.
    pub prepared_message: Option<String>,
}

pub fn query(root: &Path) -> GitStatus {
    // One probe answers both questions is_git_repo asked: exit status is
    // repo membership, stdout is the toplevel every repo-relative path
    // must resolve against.
    let Some(repo_root) = repo_toplevel(root) else {
        return GitStatus::default();
    };
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
    let porcelain =
        run_git(root, &["status", "--porcelain", "--untracked-files=all"]).unwrap_or_default();
    let dirty = parse_porcelain_dirty(&porcelain);
    let changed_count = porcelain.lines().filter(|l| !l.trim().is_empty()).count();
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
        repo_root: Some(repo_root),
        changed_count,
        prepared_message: prepared_message(root),
    }
}

/// git's prepared message for the commit in progress (#1282): `MERGE_MSG`
/// (a conflicted merge, cherry-pick or revert), else `SQUASH_MSG`, found
/// through `--git-path` so a worktree or submodule reads its own. Comment
/// lines are dropped as `commit.cleanup=strip` drops them; nothing left
/// is no message.
fn prepared_message(root: &Path) -> Option<String> {
    let paths = run_git(
        root,
        &[
            "rev-parse",
            "--git-path",
            "MERGE_MSG",
            "--git-path",
            "SQUASH_MSG",
        ],
    )
    .ok()?;
    paths.lines().find_map(|p| {
        let text = std::fs::read_to_string(root.join(p.trim())).ok()?;
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::trim_end)
            .collect();
        let message = kept.join("\n").trim().to_string();
        (!message.is_empty()).then_some(message)
    })
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
    let confirmed =
        git_raw(root, &["check-ignore", "-z", "--stdin"], Some(&stdin)).unwrap_or_default();
    confirmed
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|raw| join_raw(root, raw))
        .collect()
}

/// Whether git ignores `path` under the repository at `root`: one
/// `check-ignore` call, for a single path's verdict (#263: a save of build
/// output or a `.env` does not rerun watched tests). Outside a repository,
/// or when git cannot answer, nothing is ignored.
pub fn is_ignored(root: &Path, path: &Path) -> bool {
    let mut stdin = path.as_os_str().as_encoded_bytes().to_vec();
    stdin.push(0);
    git_raw(root, &["check-ignore", "-z", "--stdin"], Some(&stdin))
        .is_some_and(|out| out.iter().any(|b| *b != 0))
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
        // A read-only poll must never take `index.lock` out from under a user
        // mutation — same reason `run_git` sets it.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(if stdin_data.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
    let mut child = cmd.spawn().ok()?;
    // Feed stdin from its OWN thread and read stdout concurrently.
    // `check-ignore` echoes each match back as it reads, so both pipes fill
    // on a large set; writing all of stdin before reading a byte of stdout
    // deadlocks — git blocks writing stdout, stops draining stdin, and the
    // writer waits forever. Measured threshold on macOS: ~143 KB flows,
    // ~957 KB blocks.
    let writer = match stdin_data {
        Some(data) => {
            let mut pipe = child.stdin.take()?;
            let data = data.to_vec();
            Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
                // Dropping the pipe closes it, which is what tells
                // check-ignore the list is complete.
            }))
        }
        None => None,
    };
    let out = child.wait_with_output().ok();
    if let Some(w) = writer {
        let _ = w.join();
    }
    Some(out?.stdout)
}

/// The immediate subfolders of `dir` that hold a repository (a `.git`
/// entry: a folder, or a file for worktrees and submodules), sorted by
/// name. Hidden folders are skipped, like VS Code's depth-1 scan (#1227).
pub fn child_repos(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    repos.sort();
    repos
}

/// The toplevel of the repository containing `dir`, or `None` outside a
/// repo. This is the ONLY correct base for porcelain/numstat paths and
/// for rev pathspecs (`HEAD:<rel>`), which git resolves against the
/// toplevel regardless of `-C`; command-line pathspecs are cwd-relative,
/// so mutations that name a repo-relative path must run `-C` here too.
pub fn repo_toplevel(dir: &Path) -> Option<PathBuf> {
    run_git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
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

/// Keep a git child from prompting on croft's terminal. A push or pull
/// needing a password or an ssh passphrase opened `/dev/tty`, drew over
/// the full-screen UI and waited for input croft never passed on. With no
/// terminal prompt and no controlling tty, git and ssh fail at once with a
/// message the panel shows; credential helpers and agents still work.
fn never_prompt(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    #[cfg(unix)]
    detach_from_tty(cmd);
}

#[cfg(unix)]
fn detach_from_tty(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe and the only call in the
    // pre-exec hook; the forked child is never a process-group leader, so
    // the call always succeeds.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
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
    never_prompt(&mut cmd);
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
    // Porcelain and numstat paths are TOPLEVEL-relative whatever `-C`
    // names, so any filesystem read of an entry must join the toplevel,
    // not the workspace root (#139).
    let Some(toplevel) = repo_toplevel(root) else {
        return Vec::new();
    };
    let path_str = match root.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let output = match Command::new("git")
        // Poll without taking index.lock; see run_git.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", path_str, "status", "--porcelain", "--untracked-files=all"])
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
            entry.additions = count_lines_in_file(&toplevel, &entry.path);
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

/// The clean/smudge filter `rel_path` goes through (`lfs` for a Git LFS
/// file), from `git check-attr filter`. `None` when no filter applies.
pub fn path_filter(root: &Path, rel_path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-attr", "filter", "--", rel_path])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text.trim_end().rsplit(": ").next()?;
    match value {
        "" => None,
        // check-attr prints these states and the same-named values alike
        // (`filter` and `filter=set` both read `set`): only a configured
        // driver makes one a filter.
        "unspecified" | "unset" | "set" if !filter_driver_configured(root, value) => None,
        name => Some(name.to_string()),
    }
}

/// Whether git config defines a `filter.<name>` driver.
fn filter_driver_configured(root: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get-regexp"])
        .arg(format!(
            r"^filter\.{}\.(clean|smudge|process)$",
            regex::escape(name)
        ))
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
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
    let output = show_blob(Path::new(path_str), &spec, rel_path)
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

/// The `git` command that prints the blob `spec` names, for `rel_path`.
///
/// A filtered file (Git LFS) is stored in its clean form, a pointer, while
/// the working tree holds the smudged content: such a blob is read through
/// the filter (`cat-file --filters`), so every view of a past version shows
/// the file and not the pointer (#1238, #1338). Anything else is read with
/// `git show`, byte for byte as git stores it.
fn show_blob(root: &Path, spec: &str, rel_path: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root);
    match path_filter(root, rel_path) {
        Some(_) => cmd.args(["cat-file", "--filters"]),
        None => cmd.arg("show"),
    };
    cmd.arg(spec);
    cmd
}

/// The contents of `rel_path` (relative to `root`) at revision `rev`, via
/// `git show <rev>:./<rel_path>` (through the filter for a filtered file). The `./` makes the path relative to
/// `root` rather than to the repository top, so a workspace opened in a
/// subdirectory of a repo still reads the right file. `Err` when the file
/// did not exist at that revision or is not UTF-8.
pub fn read_file_at_rev(root: &Path, rev: &str, rel_path: &str) -> Result<String, String> {
    let spec = format!("{rev}:./{rel_path}");
    let output = show_blob(root, &spec, rel_path)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{rel_path} at {rev} is not UTF-8"))
}

/// Read one index stage of an unmerged path via `git show :N:<rel_path>`
/// — 1 = common ancestor (base), 2 = ours, 3 = theirs. The merge
/// editor's input source (#253). Errors when the stage does not exist
/// (e.g. no base for an added-by-both conflict, or the path is not
/// actually unmerged) or the blob is not UTF-8; the caller decides
/// whether that's fatal or means "empty side".
pub fn read_file_at_stage(root: &Path, rel_path: &str, stage: u8) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let spec = format!(":{stage}:{rel_path}");
    let output = show_blob(Path::new(path_str), &spec, rel_path)
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
    String::from_utf8(output.stdout)
        .map_err(|_| format!("{rel_path} at stage {stage} is not UTF-8"))
}

#[derive(Clone, Copy)]
enum HunkSide {
    Old,
    New,
}

/// `patch` with each `@@ -a,b +c,d @@` header's two starts set to the
/// `side` one, counts untouched.
fn retarget_hunk_starts(patch: &str, side: HunkSide) -> String {
    let fix = |line: &str| -> Option<String> {
        let rest = line.strip_prefix("@@ -")?;
        let (old, rest) = rest.split_once(" +")?;
        let (new, tail) = rest.split_once(" @@")?;
        let start = |range: &str| range.split(',').next().map(str::to_string);
        let count = |range: &str| range.split_once(',').map(|(_, c)| format!(",{c}"));
        let pick = match side {
            HunkSide::Old => start(old)?,
            HunkSide::New => start(new)?,
        };
        Some(format!(
            "@@ -{pick}{} +{pick}{} @@{tail}",
            count(old).unwrap_or_default(),
            count(new).unwrap_or_default()
        ))
    };
    // Split on `\n` only: a CRLF file's patch lines end in `\r`, which
    // `str::lines` would strip.
    patch
        .split('\n')
        .map(|l| fix(l).unwrap_or_else(|| l.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The path a single-file patch writes, from its `+++ b/<path>` header.
fn patch_path(patch: &str) -> Option<&str> {
    patch
        .split('\n')
        .find_map(|l| l.strip_prefix("+++ b/"))
        .map(|p| p.trim_end_matches('\r'))
}

/// Whether `git add` turns this path's CRLF line endings into LF in the
/// index, whatever decides it (`.gitattributes` `eol`/`text`,
/// `core.autocrlf`, `core.eol`): git cleans a probe line for the path and
/// the result is compared with the plain LF line.
fn strips_cr_on_add(root: &Path, rel: &str) -> bool {
    let hash = |input: &[u8], args: &[&str]| -> Option<String> {
        use std::io::Write as _;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["hash-object", "--stdin"])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(input).ok()?;
        let out = child.wait_with_output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let path_arg = format!("--path={rel}");
    match (
        hash(b"croft\r\n", &[&path_arg]),
        hash(b"croft\n", &["--no-filters"]),
    ) {
        (Some(cleaned), Some(lf)) => cleaned == lf,
        _ => false,
    }
}

/// `patch` with the `\r` dropped from the end of every added line.
fn strip_added_crs(patch: &str) -> String {
    patch
        .split('\n')
        .map(|l| {
            if l.starts_with('+') && !l.starts_with("+++ ") {
                l.strip_suffix('\r').unwrap_or(l)
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `patch` with a `\r` put back on every context and removed line (the
/// ones taken from HEAD), to match a CRLF working tree.
fn add_crs_to_head_lines(patch: &str) -> String {
    patch
        .split('\n')
        .map(|l| {
            let from_head = (l.starts_with(' ') || l.starts_with('-')) && !l.starts_with("--- ");
            if from_head && !l.ends_with('\r') {
                format!("{l}\r")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    // The diff's `@@ -old +new @@` counts HEAD lines on one side and
    // working-tree lines on the other, and `git apply` starts its offset
    // search at the side it writes to. A single-hunk patch lands on a target
    // where no earlier hunk was applied, so each side's number is right for
    // one target only: staging writes the index (HEAD's numbering), a revert
    // writes the working tree (its own). With the other number, a file with
    // repeated context took the patch at the wrong place.
    // `git apply` patches a file in git's clean form: `--cached` writes the
    // result into the index as it is, skipping the clean filter `git add`
    // runs, so a hunk of an LFS file put the raw content into git where a
    // pointer belongs; on the working tree the patch is matched against the
    // cleaned text (the pointer) and never applies (#1238).
    let paths = patch.lines().filter_map(|l| {
        l.strip_prefix("+++ b/")
            .or_else(|| l.strip_prefix("--- a/"))
    });
    for rel in paths {
        if let Some(filter) = path_filter(root, rel) {
            let whole = match (cached, reverse) {
                (true, false) => "stage",
                (true, true) => "unstage",
                _ => "discard",
            };
            return Err(format!(
                "{rel} uses the {filter} filter: {whole} the whole file, not a part"
            ));
        }
    }
    let retargeted = match (cached, reverse) {
        (true, false) => retarget_hunk_starts(patch, HunkSide::Old),
        (false, true) => retarget_hunk_starts(patch, HunkSide::New),
        _ => patch.to_string(),
    };
    // A path git stores LF but checks out CRLF (`eol=crlf`,
    // `core.autocrlf`) has two spellings, and the patch mixes them: its
    // added lines come from the working tree (CRLF), its context and
    // removed lines from HEAD (LF). `git apply` uses no conversion, so each
    // target needs the patch in its own spelling (#1236): the index gets
    // the added lines cleaned, as `git add` would (verbatim, a CRLF
    // checkout's `\r` made the blob mixed), and the working tree gets the
    // HEAD lines with its `\r` (without, a revert did not apply at all).
    let retargeted = match patch_path(&retargeted) {
        Some(rel) if strips_cr_on_add(root, rel) => {
            if cached {
                strip_added_crs(&retargeted)
            } else if std::fs::read(root.join(rel))
                .is_ok_and(|b| b.windows(2).any(|w| w == b"\r\n"))
            {
                add_crs_to_head_lines(&retargeted)
            } else {
                retargeted
            }
        }
        _ => retargeted,
    };
    let patch = retargeted.as_str();
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
    // A hook or signing helper that wants a terminal fails fast with its
    // own message instead of fighting croft for the screen (#1153).
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str, "commit", "-am", message]);
    never_prompt(&mut cmd);
    let output = cmd
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
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str, "push"]);
    never_prompt(&mut cmd);
    let output = cmd
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

/// The folder to run a per-file query from, and the pathspec to pass it:
/// the file's own folder and name, so git finds the repo that holds the
/// file even when `root` (the workspace) sits above it (#1227). A
/// cwd-relative pathspec is exact with `-C`. Falls back to `root` and
/// `rel_path` when that folder is gone (a deleted file's history).
fn file_cwd(root: &Path, rel_path: &str) -> (PathBuf, String) {
    let abs = root.join(rel_path);
    match (abs.parent(), abs.file_name().and_then(|n| n.to_str())) {
        (Some(dir), Some(name)) if dir.is_dir() => (dir.to_path_buf(), name.to_string()),
        _ => (root.to_path_buf(), rel_path.to_string()),
    }
}

/// Recent commits touching `rel_path`, newest first, via `git log --follow`
/// (so the history survives renames, like VS Code's Timeline). Returns an
/// empty vec when the path is untracked, the repo has no history, or git is
/// unavailable — the panel renders that as an empty state, never an error.
pub fn file_history(root: &Path, rel_path: &str, limit: usize) -> Vec<FileHistoryEntry> {
    let (dir, name) = file_cwd(root, rel_path);
    let Some(path_str) = dir.to_str() else {
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
            &name,
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
    if path_filter(root, rel_path).is_some() {
        return filtered_commit_file_diff(root, hash, rel_path);
    }
    let has_diff = |raw: &str| raw.lines().any(|l| l.starts_with("diff --git"));
    let raw = diff_text(root, &["show", hash, "--", rel_path])?;
    if has_diff(&raw) {
        return Ok(raw);
    }
    // The TIMELINE lists history with `--follow`, so a commit from before a
    // rename touched the file under its OLD name, and showing it under the
    // current one was an empty diff. The follow walk says what it was called.
    match path_at_commit(root, hash, rel_path) {
        Some(old) if old != rel_path => diff_text(root, &["show", hash, "--", &old]),
        _ => Ok(raw),
    }
}

/// [`show_commit_file_diff`] for a filtered file (#1338). `git show` diffs
/// the stored blobs, which for Git LFS are pointers whose only change is the
/// `oid` line, so the two sides are read through the filter instead and
/// diffed here, under the commit's own `git show` header, in the shape the
/// viewer parses for an unfiltered file.
fn filtered_commit_file_diff(root: &Path, hash: &str, rel_path: &str) -> Result<String, String> {
    let name = match read_file_at_rev(root, hash, rel_path) {
        Ok(_) => rel_path.to_string(),
        Err(_) => path_at_commit(root, hash, rel_path).unwrap_or_else(|| rel_path.to_string()),
    };
    let new = read_file_at_rev(root, hash, &name)?;
    // Absent before this commit (the file was added here, or this is the
    // root commit): every line is new.
    let old = read_file_at_rev(root, &format!("{hash}^"), &name).unwrap_or_default();
    let body = similar::TextDiff::from_lines(&old, &new)
        .unified_diff()
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string();
    let header = diff_text(root, &["show", "-s", hash])?;
    Ok(format!("{header}\ndiff --git a/{name} b/{name}\n{body}"))
}

/// The name `rel_path` had in commit `hash`, from the same `--follow` walk
/// [`file_history`] lists. None when the commit is not in that history.
fn path_at_commit(root: &Path, hash: &str, rel_path: &str) -> Option<String> {
    let out = run_git(
        root,
        &[
            "log",
            "--follow",
            // As deep as the TIMELINE lists (`file_history`'s 50), with room
            // to spare: a row past it cannot be clicked, and the full walk
            // of a long history was the slow part.
            "-n200",
            "--name-only",
            "--format=%x1e%H",
            "--",
            rel_path,
        ],
    )
    .ok()?;
    out.split('\x1e').find_map(|record| {
        let mut lines = record.lines();
        let full = lines.next()?.trim();
        if hash.is_empty() || !full.starts_with(hash) {
            return None;
        }
        lines
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(unquote_porcelain_path)
    })
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

/// A worktree lane: a branch and the sibling directory it lives in (#348).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeLane {
    /// Absolute path of the worktree, a SIBLING of the repo.
    pub path: PathBuf,
    /// The branch created for the lane.
    pub branch: String,
    /// The branch the lane was cut FROM, recorded at creation (#348).
    ///
    /// Not derivable later. `worktree add -b` takes no start-point, so the
    /// lane forks from whatever HEAD was; git records that only in the
    /// reflog, which is local, prunable and absent from a fresh clone — so a
    /// lane made on one machine has no base to recover on another.
    /// `merge-base` cannot help either: it needs the candidate, which is the
    /// thing being asked for. Creation is the only moment the answer is free.
    ///
    /// `None` when HEAD was detached at creation, which has no branch name
    /// to record and must not be guessed at.
    pub base: Option<String>,
}

impl WorktreeLane {
    /// Plan a lane for `name` beside `repo`, or `None` when the name has
    /// nothing usable in it.
    ///
    /// Refused rather than defaulted: a name of `"..."` would otherwise
    /// become the branch `agent/` and the directory `croft-`, and a branch
    /// named after nothing is worse than being told to pick a name.
    ///
    /// The slug is what makes both halves safe at once. git refuses a ref
    /// containing `..`, whitespace, or any of `~^:?*[\`, and a directory
    /// name carrying those is a different kind of trouble — so one pass
    /// reducing to `[a-z0-9-]` covers the ref grammar and the filesystem
    /// together, rather than sanitising twice with two chances to disagree.
    pub fn plan(repo: &Path, name: &str) -> Option<Self> {
        let slug = lane_slug(name)?;
        // A SIBLING, never a child. A worktree inside its own repo is a
        // working tree git then tries to track, and the Explorer would show
        // the lane nested in the root it was cut from.
        let parent = repo.parent()?;
        let base = repo.file_name()?.to_string_lossy();
        Some(Self {
            path: parent.join(format!("{base}-{slug}")),
            branch: format!("agent/{slug}"),
            // `plan` is pure - it runs no git. The base is filled in by
            // `add_worktree_lane`, which is already running git and is the
            // moment HEAD is still the branch being forked from.
            base: None,
        })
    }
}

/// Reduce `name` to `[a-z0-9-]`, or `None` when nothing survives.
fn lane_slug(name: &str) -> Option<String> {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs so `a  b` is `a-b` rather than `a--b`, and trim so the
    // branch never starts or ends on the separator — git refuses a ref
    // component ending in `.`, and a trailing `-` merely looks broken.
    let mut out = String::new();
    for part in mapped.split('-').filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        return None;
    }
    // Capped, because the slug becomes a DIRECTORY name as well as a branch:
    // git accepts a ref of any length, but a filesystem component past ~255
    // bytes fails at `mkdir` with "File name too long" — measured — and the
    // user would see that instead of anything about lanes. 60 is far more
    // than a lane name needs. Trimmed again because the cut can land on the
    // separator.
    let capped: String = out.chars().take(60).collect();
    let capped = capped.trim_matches('-');
    (!capped.is_empty()).then(|| capped.to_string())
}

/// Why `lane` must not be removed, or `None` when it is safe to remove.
///
/// `git worktree remove` DISCARDS a dirty tree, so this is the one check in
/// the lane flow where being wrong destroys work rather than annoying
/// someone. It answers with a reason rather than a bool so the caller can
/// tell the user what to do about it.
///
/// A tree git cannot report on is treated as unsafe: an unreadable status is
/// not evidence of cleanliness, and refusing costs a user one manual
/// `git worktree remove` while guessing costs them their work.
pub fn lane_removal_block(lane: &Path) -> Option<String> {
    let Some(path_str) = lane.to_str() else {
        return Some(format!("{} is not a readable path", lane.display()));
    };
    let output = Command::new("git")
        .args(["-C", path_str, "status", "--porcelain", "--ignored"])
        .output();
    let Ok(output) = output else {
        return Some(String::from(
            "could not run git to check for uncommitted work",
        ));
    };
    if !output.status.success() {
        return Some(format!("{path_str} does not look like a git worktree"));
    }
    let dirty = String::from_utf8_lossy(&output.stdout);
    let mut changed = 0usize;
    let mut ignored = 0usize;
    for line in dirty.lines().filter(|l| !l.trim().is_empty()) {
        // `!!` is an IGNORED entry, which `--ignored` adds and plain
        // `--porcelain` omits entirely. Counted separately because the two
        // want different words, but both must refuse: `git worktree remove`
        // deletes ignored files without a murmur, and an agent lane is
        // exactly where an uncommitted `.env`, a local database, or an
        // expensive `node_modules` lives. Measured — a `.env` in a lane was
        // destroyed while plain `--porcelain` reported the tree clean.
        if line.starts_with("!!") {
            ignored += 1;
        } else {
            changed += 1;
        }
    }
    match (changed, ignored) {
        (0, 0) => None,
        (0, n) => Some(format!(
            "this lane has {n} ignored file{} (build output, local config) that removal would \
             delete — clear them by hand first",
            if n == 1 { "" } else { "s" }
        )),
        (1, _) => Some(String::from(
            "this lane has 1 file with uncommitted changes — commit or discard it first",
        )),
        (n, _) => Some(format!(
            "this lane has {n} files with uncommitted changes — commit or discard them first"
        )),
    }
}

/// Run a MUTATING git command, returning git's own error text.
///
/// Separate from the read-only `run_git` above, which goes out of its way to
/// avoid taking `.git/index.lock` because `status` and `diff` opportunistically
/// rewrite the index. A worktree add or remove genuinely changes the repo and
/// must take the lock like any other write.
fn run_git_mut(dir: &Path, args: &[&str]) -> Result<(), String> {
    let Some(dir) = dir.to_str() else {
        return Err(String::from("path is not valid UTF-8"));
    };
    let output = Command::new("git")
        .args(["-C", dir])
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    // git's own message, which names the actual problem — a branch that
    // exists, a path in use, a repo with no commits — far better than any
    // sentence this could invent.
    let err = String::from_utf8_lossy(&output.stderr);
    Err(err
        .trim()
        .lines()
        .next()
        .unwrap_or("git failed")
        .to_string())
}

/// Add `lane` as a worktree of `repo` on a new branch.
pub fn add_worktree_lane(repo: &Path, lane: &mut WorktreeLane) -> Result<(), String> {
    let Some(path) = lane.path.to_str().map(str::to_owned) else {
        return Err(String::from("lane path is not valid UTF-8"));
    };
    // READ before the command, while HEAD is still the branch the lane forks
    // from - afterwards it is gone, see `WorktreeLane::base`. But ASSIGN only
    // once creation succeeded: a failed `worktree add` leaves no lane, and a
    // base recorded on one that does not exist is a fact about nothing that a
    // retry would then carry.
    let base = current_branch(repo);
    run_git_mut(repo, &["worktree", "add", "-b", &lane.branch, &path])?;
    lane.base = base;
    Ok(())
}

/// The branch HEAD is on, or `None` when detached.
///
/// `--quiet` so a detached HEAD is an empty result rather than an error the
/// caller has to distinguish from a failure.
pub fn current_branch(root: &Path) -> Option<String> {
    let out = run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Remove `lane`'s worktree.
///
/// No `--force`: the caller has already asked [`lane_removal_block`], and a
/// force here would defeat that check by making the refusal advisory. If git
/// refuses anyway, the reason reaches the user rather than being overridden.
pub fn remove_worktree_lane(lane: &Path) -> Result<(), String> {
    let Some(path) = lane.to_str() else {
        return Err(String::from("lane path is not valid UTF-8"));
    };
    // Run from the lane itself: git resolves the main worktree from there,
    // so the caller does not have to remember which repo it was cut from.
    run_git_mut(lane, &["worktree", "remove", path])
}

/// Where review mode checks pull request `number` out (#365): a sibling of
/// `repo`, like a worktree lane, never a child git would then see.
pub fn pr_worktree_path(repo: &Path, number: u64) -> Option<PathBuf> {
    let name = repo.file_name()?.to_str()?;
    Some(repo.parent()?.join(format!("{name}-pr-{number}")))
}

/// Which remote to fetch `owner/repo`'s pull requests from: the one whose
/// URL names it (`git@github.com:o/r.git`, `https://github.com/o/r`), else
/// the repository's own URL, which `git fetch` takes as well.
pub fn remote_for_slug(repo: &Path, slug: &str) -> String {
    let slug = slug.to_ascii_lowercase();
    let names = |url: &str| {
        let url = url.trim().to_ascii_lowercase();
        let url = url.strip_suffix(".git").unwrap_or(&url).to_string();
        url.ends_with(&format!("/{slug}")) || url.ends_with(&format!(":{slug}"))
    };
    run_git(repo, &["remote", "-v"])
        .ok()
        .and_then(|out| {
            out.lines().find_map(|l| {
                let mut parts = l.split_whitespace();
                let (name, url) = (parts.next()?, parts.next()?);
                names(url).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| format!("https://github.com/{slug}.git"))
}

/// Whether `path` is one of `repo`'s worktrees.
fn is_worktree_of(repo: &Path, path: &Path) -> bool {
    let want = path.canonicalize().ok();
    run_git(repo, &["worktree", "list", "--porcelain"]).is_ok_and(|out| {
        out.lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|w| Path::new(w).canonicalize().ok() == want)
    })
}

/// A pull request head checked out by [`checkout_pr_worktree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrWorktree {
    /// The commit checked out.
    pub oid: String,
    /// Whether this call created the worktree. One that was already there
    /// is the user's as much as review mode's, and leaving never removes it.
    pub created: bool,
}

/// Fetch pull request `number` from `remote` and check its head out,
/// detached, in a worktree at `path` (#365).
///
/// An existing worktree of the same repository is moved to the new head
/// only when that loses nothing: it is clean, and its HEAD is the fetched
/// head already or `previous`, the head review mode checked out there last.
/// Any other HEAD holds commits that a detached checkout would leave to the
/// reflog. Anything else already at `path` is refused, never touched.
pub fn checkout_pr_worktree(
    repo: &Path,
    remote: &str,
    number: u64,
    path: &Path,
    previous: Option<&str>,
) -> Result<PrWorktree, String> {
    let Some(path_s) = path.to_str() else {
        return Err(String::from("worktree path is not valid UTF-8"));
    };
    let Some(repo_s) = repo.to_str() else {
        return Err(String::from("repository path is not valid UTF-8"));
    };
    // No prompt: this runs off the UI thread, where a credential prompt,
    // or ssh's own passphrase or host-key question on /dev/tty, would wait
    // forever on a terminal nobody sees, or draw over the UI.
    let mut fetch = Command::new("git");
    fetch
        .args(["-C", repo_s, "fetch", "--quiet", remote])
        .arg(format!("refs/pull/{number}/head"));
    never_prompt(&mut fetch);
    let fetch = fetch.output().map_err(|e| e.to_string())?;
    if !fetch.status.success() {
        let err = String::from_utf8_lossy(&fetch.stderr);
        return Err(err
            .trim()
            .lines()
            .next()
            .unwrap_or("git fetch failed")
            .to_string());
    }
    let oid = run_git(repo, &["rev-parse", "FETCH_HEAD"])
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    if path.exists() {
        if !is_worktree_of(repo, path) {
            return Err(format!(
                "{path_s} already exists and is not a worktree of this repository"
            ));
        }
        let head = worktree_head(path);
        if head.as_deref() != Some(oid.as_str()) {
            if let Some(why) = lane_removal_block(path) {
                return Err(format!("the existing checkout has work in it: {why}"));
            }
            if previous.is_none() || head.as_deref() != previous {
                return Err(format!(
                    "{path_s} is at a commit review mode did not check out — put that work on a branch, or remove the worktree"
                ));
            }
        }
        run_git_mut(path, &["checkout", "--quiet", "--detach", &oid])?;
        Ok(PrWorktree {
            oid,
            created: false,
        })
    } else {
        run_git_mut(repo, &["worktree", "add", "--detach", path_s, &oid])?;
        Ok(PrWorktree { oid, created: true })
    }
}

/// The commit a worktree's HEAD is on.
pub fn worktree_head(path: &Path) -> Option<String> {
    run_git(path, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Why leaving review must keep the pull request's checkout: uncommitted or
/// ignored files (see [`lane_removal_block`]), or commits made on top of the
/// checked-out head, which a detached worktree would leave unreachable.
pub fn pr_worktree_keep_reason(path: &Path, checked_out: &str) -> Option<String> {
    if let Some(why) = lane_removal_block(path) {
        return Some(why);
    }
    match worktree_head(path) {
        Some(head) if head == checked_out => None,
        Some(_) => Some(String::from(
            "it has commits beyond the pull request's head — put them on a branch first",
        )),
        None => Some(String::from("git cannot read its HEAD")),
    }
}

/// The current branch's first-parent history, newest first (#371).
///
/// NOT [`commit_graph`], which logs `--branches --tags HEAD` to draw the
/// repo-wide graph: with any other branch present that list interleaves
/// commits the current branch does not contain, and topological order can
/// put one of them at index 0. A scrubber built on it steps back from the
/// working tree onto a commit that is not HEAD and may not even be an
/// ancestor of it.
///
/// `--first-parent` because a scrubber walks the branch as a line. A merge's
/// second parent is a different line of development, and stepping into one
/// would take the user somewhere they cannot step back out of in a straight
/// line.
///
/// Empty on any failure — no repo, no commits, no git — so the caller shows
/// an empty state rather than an error, matching `commit_graph`.
pub fn branch_history(root: &Path, limit: usize) -> Vec<GraphCommit> {
    branch_history_range(root, "HEAD", limit)
}

/// [`branch_history`] over an explicit revision spec rather than `HEAD`.
///
/// Takes the spec as a string so a caller can pass a range: `base..lane`
/// lists the commits a lane added and none of the base's, which is what
/// "the branch's diff against the base" means for a worktree lane (#348).
/// `HEAD` reproduces the unranged behaviour exactly, which is why
/// `branch_history` is now a one-line call rather than a second copy of
/// this body.
///
/// Empty on any failure, like its caller: an unknown revision is a git
/// error, and the graph shows an empty state rather than an error row.
pub fn branch_history_range(root: &Path, revspec: &str, limit: usize) -> Vec<GraphCommit> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--first-parent",
            revspec,
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    parse_commit_graph(&String::from_utf8_lossy(&output.stdout), now)
}

/// Commit `rev`'s tree under a workspace root, as [`tree_paths`] lists it
/// for the Explorer's dimming while the scrubber sits on a commit (#371).
#[derive(Debug, Default)]
pub struct CommitTree {
    /// Every path in the tree, absolute, directories included.
    pub paths: HashSet<PathBuf>,
    /// Submodule (gitlink) paths. `ls-tree` never lists their contents, so
    /// anything beneath one counts as present rather than dimming a whole
    /// submodule checkout.
    pub submodules: Vec<PathBuf>,
}

impl CommitTree {
    /// Whether `path` existed in the commit: listed, or inside a submodule.
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path) || self.submodules.iter().any(|s| path.starts_with(s))
    }
}

/// Every path in commit `rev`'s tree under `root`, absolute (`root`
/// joined), directories included — what the Explorer dims against while
/// the scrubber sits on a commit (#371).
///
/// `ls-tree` run with `-C root` and no `--full-tree` lists only the
/// entries under `root` and prints them relative to it, the same base
/// [`read_file_at_rev`]'s `./` gives, so a workspace opened in a
/// subdirectory of a repo joins onto its own root rather than the
/// toplevel. `-t` emits each directory on the way down, so a folder that
/// existed keeps its normal colour without deriving ancestors here.
///
/// `None` on any failure (no repo, unknown revision), so a caller dims
/// nothing rather than everything.
pub fn tree_paths(root: &Path, rev: &str) -> Option<CommitTree> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["ls-tree", "-r", "-t", "-z", rev])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut tree = CommitTree::default();
    // Each entry is `<mode> <type> <object>\t<path>`.
    for entry in output.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let path = join_raw(root, &entry[tab + 1..]);
        if entry[..tab].split(|b| *b == b' ').nth(1) == Some(b"commit") {
            tree.submodules.push(path.clone());
        }
        tree.paths.insert(path);
    }
    Some(tree)
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
    /// The line's text as blamed, when known: the annotation is shown only
    /// while the buffer's line still reads the same (see
    /// `Editor::committed_blame_annotation`).
    pub text: Option<String>,
}

/// Per-line blame for `rel_path`, indexed 0-based by result position (result
/// `[0]` is line 1). Uses `git blame --line-porcelain` so each line carries
/// its author, author-time, and summary. Returns empty on any failure so the
/// caller renders no annotation rather than an error.
pub fn blame(root: &Path, rel_path: &str) -> Vec<BlameLine> {
    let (dir, name) = file_cwd(root, rel_path);
    let Some(path_str) = dir.to_str() else {
        return Vec::new();
    };
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", path_str, "blame", "--line-porcelain", "--", &name])
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
        } else if let Some(content) = raw.strip_prefix('\t') {
            // The content line closes a group: emit the accumulated blame.
            if let Some(full) = hash.take() {
                let uncommitted = full.chars().all(|c| c == '0');
                lines.push(BlameLine {
                    short_hash: full.chars().take(8).collect(),
                    summary: std::mem::take(&mut summary),
                    author: std::mem::take(&mut author),
                    age_secs: now - author_time,
                    uncommitted,
                    text: Some(content.to_string()),
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

/// `git rm` one path: out of the index and off the disk. Resolves a
/// modify/delete conflict in favour of the deletion (#1244).
pub fn remove_path(root: &Path, rel_path: &str) -> Result<(), String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let out = Command::new("git")
        .args([
            "-C",
            path_str,
            "--literal-pathspecs",
            "rm",
            "-q",
            "-f",
            "--",
        ])
        .arg(rel_path)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
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
    // Literal: git reads a pathspec as a glob, so staging Next.js's
    // `pages/[id].tsx` also staged `pages/i.tsx` and untracked `pages/d.tsx`.
    cmd.args(["-C", path_str, "--literal-pathspecs", "add", "--"]);
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
    let mut args: Vec<&str> = vec!["--literal-pathspecs", "reset", "-q", "HEAD", "--"];
    args.extend(rel_paths.iter().map(String::as_str));
    run_mutation(root, &args).map(|_| ())
}

/// Discard a single path. A Changes row restores the working tree from the
/// INDEX (`git checkout -- <path>`), so a staged part of the same file is
/// kept, as VS Code does; restoring from HEAD threw the staged work away
/// too. A Staged row restores both from HEAD. Untracked entries are removed
/// from disk. Destructive: callers
/// MUST confirm with the user before invoking — the Source Control panel
/// shows a Y/N modal before reaching this function.
pub fn discard_path(
    root: &Path,
    rel_path: &str,
    untracked: bool,
    staged: bool,
) -> Result<(), String> {
    if untracked {
        let abs = root.join(rel_path);
        if abs.is_dir() {
            // Porcelain folds a directory holding any untracked file into one
            // `?? dir/` row; `remove_dir_all` took the IGNORED files inside
            // with it (`.env`, `node_modules/`). `git clean` removes only
            // the untracked ones.
            run_mutation(
                root,
                &["--literal-pathspecs", "clean", "-fdq", "--", rel_path],
            )
            .map(|_| ())
        } else {
            std::fs::remove_file(&abs)
                .map_err(|e| format!("failed to remove {}: {e}", abs.display()))
        }
    } else {
        let path_str = root
            .to_str()
            .ok_or_else(|| "non-utf8 workspace path".to_string())?;
        let mut cmd = Command::new("git");
        cmd.args(["-C", path_str, "--literal-pathspecs", "checkout"]);
        if staged {
            cmd.arg("HEAD");
        }
        let output = cmd
            .args(["--", rel_path])
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
    // `--`: a URL starting with `-` (`--upload-pack=<command>`) is
    // otherwise an option, and that one runs a command.
    run_mutation(parent, &["clone", "--", url, &name]).map(|_| dest)
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

/// Undo Last Commit (#1350), as VS Code does it: `git reset --soft HEAD~1`,
/// so HEAD moves to the parent and the commit's changes stay staged, or
/// on the root commit `git update-ref -d HEAD`, which leaves the branch
/// unborn with every file staged. Returns the undone commit's message, for
/// the message box. Refused while a merge, rebase, cherry-pick or revert
/// is in progress: moving HEAD under one of those breaks it.
pub fn undo_last_commit(root: &Path) -> Result<String, String> {
    if let Some(op) = operation_in_progress(root) {
        return Err(format!(
            "a {op} is in progress; finish or abort it before undoing a commit"
        ));
    }
    let message = run_git(root, &["log", "-1", "--format=%B", "HEAD"])
        .map_err(|_| String::from("there is no commit to undo"))?;
    if run_git(root, &["rev-parse", "--verify", "-q", "HEAD~1"]).is_ok() {
        run_mutation(root, &["reset", "--soft", "HEAD~1"])?;
    } else {
        // A shallow checkout cuts history at a commit that still has a
        // parent: deleting the ref there would drop the branch tip.
        let raw = run_git(root, &["cat-file", "commit", "HEAD"])
            .map_err(|_| String::from("cannot read the last commit"))?;
        if raw
            .lines()
            .take_while(|l| !l.is_empty())
            .any(|l| l.starts_with("parent "))
        {
            return Err(String::from(
                "the parent commit is not in this shallow checkout; deepen it before undoing a commit",
            ));
        }
        // On a detached HEAD, `update-ref -d HEAD` deletes `.git/HEAD`
        // itself and the repository stops being one.
        if run_git(root, &["symbolic-ref", "-q", "HEAD"]).is_err() {
            return Err(String::from(
                "HEAD is detached; check out a branch before undoing its root commit",
            ));
        }
        run_mutation(root, &["update-ref", "-d", "HEAD"])?;
    }
    Ok(message)
}

/// The git operation in progress in `root`'s repository, by the state
/// file git keeps for it, or `None`.
fn operation_in_progress(root: &Path) -> Option<&'static str> {
    const STATES: [(&str, &str); 5] = [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
    ];
    let mut args = vec!["rev-parse"];
    for (file, _) in STATES {
        args.extend(["--git-path", file]);
    }
    let paths = run_git(root, &args).ok()?;
    paths
        .lines()
        .zip(STATES)
        .find(|(path, _)| root.join(path.trim()).exists())
        .map(|(_, (_, op))| op)
}

/// Whether HEAD is already on its upstream (`HEAD` is an ancestor of
/// `@{u}`), so undoing it rewrites published history and needs a force
/// push later (#1350). No upstream is not pushed; an upstream that is
/// configured but cannot be checked (its tracking ref missing) counts as
/// pushed, so the warning errs on the side of showing.
pub fn head_is_on_upstream(root: &Path) -> bool {
    let code = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", "HEAD", "@{u}"])
        .output()
        .ok()
        .and_then(|o| o.status.code());
    match code {
        Some(0) => true,
        Some(1) => false,
        _ => run_git(root, &["symbolic-ref", "-q", "HEAD"])
            .and_then(|branch| run_git(root, &["for-each-ref", "--format=%(upstream)", &branch]))
            .is_ok_and(|upstream| !upstream.is_empty()),
    }
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

/// Publish the current branch: push it and set upstream tracking
/// (`git push -u <remote> <branch>`, the remote chosen by
/// [`publish_remote`]). Used when a local branch has no upstream yet.
pub fn publish_branch(root: &Path, branch: &str) -> Result<String, String> {
    let remote = publish_remote(root)?;
    run_mutation(root, &["push", "-u", &remote, branch])?;
    Ok(format!("{branch} to {remote}"))
}

/// The current branch when it has no upstream to push to or pull from
/// (`@{u}` does not resolve), the case `git push` refuses with "has no
/// upstream branch". None on a tracked branch and on a detached HEAD.
pub fn unpublished_branch(root: &Path) -> Option<String> {
    let branch = run_git(root, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(parse_branch)?;
    let tracked = run_git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok();
    (!tracked).then_some(branch)
}

/// The remote an unpublished branch goes to: `remote.pushDefault` when
/// set, else `origin`, else the only remote. With several remotes and
/// none of those, picking one would be a guess, so it is an error.
fn publish_remote(root: &Path) -> Result<String, String> {
    if let Ok(remote) = run_git(root, &["config", "--get", "remote.pushDefault"])
        && !remote.is_empty()
    {
        return Ok(remote);
    }
    let listed = run_git(root, &["remote"]).map_err(|e| e.to_string())?;
    let remotes: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    match remotes.as_slice() {
        [] => Err(String::from("no remote to publish this branch to")),
        [only] => Ok((*only).to_string()),
        several if several.contains(&"origin") => Ok(String::from("origin")),
        _ => Err(String::from(
            "branch has no upstream and several remotes, none of them origin: set remote.pushDefault to pick one",
        )),
    }
}

/// Push the current branch, publishing it first if it has no upstream
/// (`git push -u <remote> <branch>`), as VS Code's Push and Sync do. A
/// plain `git push` would refuse with "has no upstream branch".
pub fn push_or_publish(root: &Path) -> Result<String, String> {
    let Some(branch) = unpublished_branch(root) else {
        return push_current_branch(root);
    };
    publish_branch(root, &branch).map(|summary| format!("published {summary}"))
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

    /// #1236: `run.bat` stored LF in the index and CRLF in the working tree,
    /// with `echo two` changed to `echo TWO`; `config` is extra setup.
    fn crlf_checkout_with_one_edit(root: &Path, attrs: &str, config: &[(&str, &str)]) -> String {
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "a@b"]);
        git(&["config", "user.name", "a"]);
        for (k, v) in config {
            git(&["config", k, v]);
        }
        std::fs::write(root.join(".gitattributes"), attrs).unwrap();
        let head = "echo one\r\necho two\r\necho three\r\necho four\r\n";
        std::fs::write(root.join("run.bat"), head).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let work = head.replace("two", "TWO");
        std::fs::write(root.join("run.bat"), &work).unwrap();
        let head = read_file_at_head(root, "run.bat").unwrap();
        let diff = crate::widgets::diff::DiffData::build_with_byte_check(
            std::path::PathBuf::new(),
            root.join("run.bat"),
            head.lines().map(str::to_string).collect(),
            work.lines().map(str::to_string).collect(),
            Some(&head),
            Some(&work),
        );
        diff.hunk_patch("run.bat", diff.hunk_range_at(0).unwrap())
    }

    #[test]
    fn staging_a_hunk_of_an_eol_crlf_file_stores_lf_like_git_add() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let patch = crlf_checkout_with_one_edit(root, "*.bat text eol=crlf\n", &[]);
        apply_patch(root, &patch, true, false).unwrap();
        let eol = git_stdout(root, &["ls-files", "--eol", "run.bat"]);
        assert!(eol.starts_with("i/lf "), "the index blob: {eol}");
        let status = git_stdout(root, &["status", "--porcelain", "run.bat"]);
        assert_eq!(status, "M  run.bat\n", "nothing is left unstaged");
        // And back out again: the unstage patch must match the LF index.
        apply_patch(root, &patch, true, true).unwrap();
        assert_eq!(git_stdout(root, &["diff", "--cached"]), "");
    }

    #[test]
    fn staging_a_hunk_under_core_autocrlf_stores_lf_like_git_add() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let patch = crlf_checkout_with_one_edit(root, "", &[("core.autocrlf", "true")]);
        apply_patch(root, &patch, true, false).unwrap();
        let eol = git_stdout(root, &["ls-files", "--eol", "run.bat"]);
        assert!(eol.starts_with("i/lf "), "the index blob: {eol}");
    }

    #[test]
    fn staging_a_hunk_of_a_file_kept_as_crlf_keeps_its_crs() {
        // Negative: with no conversion configured, the index holds CRLF and
        // `git add` keeps it, so the staged line keeps its `\r` too.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let patch = crlf_checkout_with_one_edit(root, "", &[("core.autocrlf", "false")]);
        apply_patch(root, &patch, true, false).unwrap();
        let eol = git_stdout(root, &["ls-files", "--eol", "run.bat"]);
        assert!(eol.starts_with("i/crlf "), "the index blob: {eol}");
        assert_eq!(
            git_stdout(root, &["status", "--porcelain", "run.bat"]),
            "M  run.bat\n"
        );
    }

    #[test]
    fn reverting_a_hunk_of_an_eol_crlf_file_writes_crlf_to_the_work_tree() {
        // Negative: a revert writes the working tree, which keeps CRLF.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let patch = crlf_checkout_with_one_edit(root, "*.bat text eol=crlf\n", &[]);
        apply_patch(root, &patch, false, true).unwrap();
        assert_eq!(
            std::fs::read(root.join("run.bat")).unwrap(),
            b"echo one\r\necho two\r\necho three\r\necho four\r\n"
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
    fn git_raw_does_not_deadlock_on_a_payload_past_the_pipe_buffer() {
        // check-ignore echoes each match back as it reads, so BOTH pipes
        // fill. Writing all of stdin before reading a byte of stdout
        // deadlocks once the payload passes the buffer: git blocks writing
        // stdout, stops draining stdin, and the writer blocks forever.
        // Measured on this machine: 143 KB flows, 957 KB blocks — so this
        // uses ~1 MB. On the single-threaded version this test hangs.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        std::fs::write(p.join(".gitignore"), "*.log\n").unwrap();
        let n = 20_000;
        let mut payload = Vec::new();
        for i in 0..n {
            payload.extend_from_slice(
                format!("padded-name-to-widen-the-pipe-payload-{i:06}.log").as_bytes(),
            );
            payload.push(0);
        }
        let out =
            git_raw(p, &["check-ignore", "-z", "--stdin"], Some(&payload)).unwrap_or_default();
        let got = out.split(|b| *b == 0).filter(|s| !s.is_empty()).count();
        assert_eq!(
            got, n,
            "every path must come back instead of stalling on a full pipe"
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
    fn one_paths_ignore_verdict_matches_the_rules() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["init", "-q", "-b", "main"])
            .output();
        std::fs::write(p.join(".gitignore"), "*.log\nout/\n").unwrap();
        assert!(is_ignored(p, &p.join("a.log")));
        assert!(
            is_ignored(p, &p.join("out/deep/gen.rs")),
            "under an ignored dir"
        );
        assert!(!is_ignored(p, &p.join("src/lib.rs")));
        let bare = TempDir::new().unwrap();
        assert!(
            !is_ignored(bare.path(), &bare.path().join("a.log")),
            "no repo"
        );
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

    /// GIT_TERMINAL_PROMPT and a null stdin are checked too, but the session
    /// is what tells: a test runner has no terminal to hand on and may set
    /// GIT_TERMINAL_PROMPT=0 itself, while a hook still in croft's session
    /// can open croft's `/dev/tty`.
    #[cfg(target_os = "linux")]
    #[test]
    fn commit_all_tracked_never_offers_its_hooks_the_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(p)
                .args(args)
                .output()
                .unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "a@b"]);
        git(&["config", "user.name", "a"]);
        std::fs::write(p.join("f.txt"), "one\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-qm", "init"]);
        let hook = p.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            "#!/bin/sh\necho \"prompt=$GIT_TERMINAL_PROMPT\" > seen\n\
             if [ -t 0 ]; then echo stdin=tty >> seen; else echo stdin=none >> seen; fi\n\
             if [ \"$(cut -d' ' -f6 /proc/$$/stat)\" = \"$(cat croft_sid)\" ]; \
             then echo session=croft >> seen; else echo session=own >> seen; fi\n",
        )
        .unwrap();
        // SAFETY: getsid(0) only reads the calling process's session id.
        let sid = unsafe { libc::getsid(0) };
        std::fs::write(p.join("croft_sid"), sid.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(p.join("f.txt"), "two\n").unwrap();
        commit_all_tracked(p, "two").unwrap();
        assert_eq!(
            std::fs::read_to_string(p.join("seen")).unwrap(),
            "prompt=0\nstdin=none\nsession=own\n"
        );
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

    fn sh_git(p: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn staging_a_bracketed_path_stages_only_that_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::create_dir(p.join("pages")).unwrap();
        std::fs::write(p.join("pages/[id].tsx"), "a").unwrap();
        std::fs::write(p.join("pages/i.tsx"), "b").unwrap();
        stage_paths(p, &[String::from("pages/[id].tsx")]).unwrap();
        let staged = sh_git(p, &["diff", "--cached", "--name-only"]);
        assert_eq!(staged.trim(), "pages/[id].tsx");
        unstage_paths(p, &[String::from("pages/[id].tsx")]).unwrap();
        assert_eq!(sh_git(p, &["diff", "--cached", "--name-only"]).trim(), "");
    }

    #[test]
    fn discarding_a_changes_row_keeps_the_staged_part() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join("seed.txt"), "staged\n").unwrap();
        sh_git(p, &["add", "seed.txt"]);
        std::fs::write(p.join("seed.txt"), "staged\nunstaged\n").unwrap();
        discard_path(p, "seed.txt", false, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(p.join("seed.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(
            sh_git(p, &["diff", "--cached", "--name-only"]).trim(),
            "seed.txt"
        );
    }

    #[test]
    fn discarding_an_untracked_folder_keeps_its_ignored_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join(".gitignore"), ".env\n").unwrap();
        sh_git(p, &["add", ".gitignore"]);
        sh_git(p, &["commit", "-qm", "ignore"]);
        std::fs::create_dir(p.join("app")).unwrap();
        std::fs::write(p.join("app/new.rs"), "x").unwrap();
        std::fs::write(p.join("app/.env"), "SECRET=1").unwrap();
        discard_path(p, "app/", true, false).unwrap();
        assert!(!p.join("app/new.rs").exists());
        assert!(
            p.join("app/.env").exists(),
            "an ignored file is not the discard's to delete"
        );
    }

    #[test]
    fn a_single_hunk_stages_and_reverts_at_its_own_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        // Repeated context, and an earlier unstaged hunk that shifts every
        // later working-tree line by three.
        let head: String = (0..12)
            .map(|i| if i % 2 == 0 { "a\n" } else { "c\n" })
            .collect();
        std::fs::write(p.join("f.txt"), &head).unwrap();
        sh_git(p, &["add", "f.txt"]);
        sh_git(p, &["commit", "-qm", "f"]);
        let mut work: Vec<&str> = head.lines().collect();
        work.insert(9, "cz");
        for _ in 0..3 {
            work.insert(0, "top");
        }
        let work_text = work.join("\n") + "\n";
        std::fs::write(p.join("f.txt"), &work_text).unwrap();
        // The later hunk alone: `cz` after HEAD line 9, context a/c around it.
        let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -9,2 +12,3 @@\n a\n+cz\n c\n";
        apply_patch(p, patch, true, false).unwrap();
        let staged = sh_git(p, &["show", ":f.txt"]);
        let mut want: Vec<&str> = head.lines().collect();
        want.insert(9, "cz");
        assert_eq!(staged, want.join("\n") + "\n", "staged at HEAD line 9");
        sh_git(p, &["reset", "-q"]);
        apply_patch(p, patch, false, true).unwrap();
        let reverted = std::fs::read_to_string(p.join("f.txt")).unwrap();
        let mut want: Vec<&str> = work_text.lines().collect();
        want.remove(12);
        assert_eq!(
            reverted,
            want.join("\n") + "\n",
            "reverted at working line 12"
        );
    }

    /// #365: review mode's checkout fetches `refs/pull/N/head` into a
    /// detached sibling worktree, moves it on a re-run after a new push,
    /// refuses a directory that is not its worktree, and says why leaving
    /// must keep a checkout with edits or new commits in it.
    #[test]
    fn a_pull_request_head_checks_out_into_a_sibling_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
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
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let up = tmp.path().join("up");
        std::fs::create_dir(&up).unwrap();
        init_repo_with_commit(&up);
        let pr_commit = |msg: &str| {
            std::fs::write(up.join("pr.txt"), msg).unwrap();
            git(&up, &["add", "."]);
            git(&up, &["commit", "-q", "-m", msg]);
            let sha = git(&up, &["rev-parse", "HEAD"]);
            git(&up, &["update-ref", "refs/pull/7/head", &sha]);
            sha
        };
        let first = pr_commit("one");
        let app = tmp.path().join("app");
        git(tmp.path(), &["clone", "-q", up.to_str().unwrap(), "app"]);
        git(&app, &["config", "user.email", "a@b"]);
        git(&app, &["config", "user.name", "a"]);

        let path = pr_worktree_path(&app, 7).unwrap();
        assert_eq!(path, tmp.path().join("app-pr-7"));
        let remote = remote_for_slug(&app, "o/r");
        assert_eq!(remote, "https://github.com/o/r.git", "no remote names o/r");
        assert_eq!(
            checkout_pr_worktree(&app, "origin", 7, &path, None),
            Ok(PrWorktree {
                oid: first.clone(),
                created: true
            })
        );
        assert_eq!(std::fs::read_to_string(path.join("pr.txt")).unwrap(), "one");
        assert_eq!(pr_worktree_keep_reason(&path, &first), None);

        // A new push moves the checkout review mode made, and only that:
        // not knowing what it last checked out there, it refuses.
        let second = pr_commit("two");
        let err = checkout_pr_worktree(&app, "origin", 7, &path, None).unwrap_err();
        assert!(err.contains("did not check out"), "{err}");
        assert_eq!(
            checkout_pr_worktree(&app, "origin", 7, &path, Some(&first)),
            Ok(PrWorktree {
                oid: second.clone(),
                created: false
            })
        );
        assert_eq!(std::fs::read_to_string(path.join("pr.txt")).unwrap(), "two");

        // Edits and new commits are reasons to keep it.
        std::fs::write(path.join("pr.txt"), "edited").unwrap();
        assert!(pr_worktree_keep_reason(&path, &second).is_some());
        git(&path, &["commit", "-q", "-am", "mine"]);
        let why = pr_worktree_keep_reason(&path, &second).expect("a new commit keeps it");
        assert!(why.contains("commits"), "{why}");
        // ...and a refresh will not detach away from that commit either.
        let mine = git(&path, &["rev-parse", "HEAD"]);
        let third = pr_commit("three");
        let err = checkout_pr_worktree(&app, "origin", 7, &path, Some(&second)).unwrap_err();
        assert!(err.contains("did not check out"), "{err}");
        assert_eq!(worktree_head(&path).as_deref(), Some(mine.as_str()));
        assert_ne!(mine, third);

        // Something else at the path is never touched.
        let stray = pr_worktree_path(&app, 8).unwrap();
        std::fs::create_dir(&stray).unwrap();
        let second_pr = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/8/head", &second_pr]);
        let err = checkout_pr_worktree(&app, "origin", 8, &stray, None).unwrap_err();
        assert!(err.contains("not a worktree"), "{err}");
    }

    #[test]
    fn the_pull_request_remote_is_the_one_naming_the_repository() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        for (name, url) in [
            ("origin", "git@github.com:me/fork.git"),
            ("upstream", "https://github.com/Owner/Repo"),
        ] {
            let _ = Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(["remote", "add", name, url])
                .status();
        }
        assert_eq!(remote_for_slug(tmp.path(), "owner/repo"), "upstream");
        assert_eq!(remote_for_slug(tmp.path(), "me/fork"), "origin");
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

    /// `git -C p <args>`, asserting it succeeded; its stdout, trimmed.
    fn git_in(p: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// #1350: Undo Last Commit is `reset --soft HEAD~1`: HEAD goes back to
    /// the parent, the commit's changes are staged, and its message comes
    /// back for the box.
    #[test]
    fn undo_last_commit_moves_head_back_and_keeps_the_changes_staged() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let parent = git_in(p, &["rev-parse", "HEAD"]);
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        git_in(p, &["add", "b.txt"]);
        git_in(p, &["commit", "-q", "-m", "Add b\n\nWith a body."]);
        assert_eq!(undo_last_commit(p).as_deref(), Ok("Add b\n\nWith a body."));
        assert_eq!(git_in(p, &["rev-parse", "HEAD"]), parent);
        assert_eq!(git_in(p, &["diff", "--cached", "--name-only"]), "b.txt");
        assert_eq!(std::fs::read_to_string(p.join("b.txt")).unwrap(), "b\n");
    }

    /// #1350: undoing the root commit leaves the branch unborn with every
    /// file staged, as VS Code does with `update-ref -d HEAD`.
    #[test]
    fn undo_last_commit_on_the_root_commit_leaves_an_unborn_branch_staged() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        assert_eq!(undo_last_commit(p).as_deref(), Ok("init"));
        let head = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .output()
            .unwrap();
        assert!(!head.status.success(), "the branch is unborn");
        assert_eq!(git_in(p, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(git_in(p, &["diff", "--cached", "--name-only"]), "seed.txt");
    }

    /// #1350 negative: in the middle of a merge, rebase, cherry-pick or
    /// revert the undo is refused, says why, and leaves HEAD alone.
    #[test]
    fn undo_last_commit_refuses_during_a_merge() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let head = git_in(p, &["rev-parse", "HEAD"]);
        std::fs::write(p.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
        let err = undo_last_commit(p).unwrap_err();
        assert!(err.contains("merge"), "{err}");
        assert_eq!(git_in(p, &["rev-parse", "HEAD"]), head);
    }

    /// #1350 negative: a repository with no commit has nothing to undo.
    #[test]
    fn undo_last_commit_with_no_commit_says_so() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        git_in(p, &["init", "-q", "-b", "main"]);
        let err = undo_last_commit(p).unwrap_err();
        assert!(err.contains("no commit"), "{err}");
    }

    /// #1350: whether HEAD is already on its upstream, so the app can warn
    /// that undoing it means a force push later. No upstream is not pushed.
    #[test]
    fn head_on_upstream_follows_the_tracking_branch() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        assert!(!head_is_on_upstream(p), "no upstream yet");
        let remote = TempDir::new().unwrap();
        git_in(remote.path(), &["init", "-q", "--bare"]);
        git_in(
            p,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git_in(p, &["push", "-q", "-u", "origin", "main"]);
        assert!(head_is_on_upstream(p), "pushed");
        std::fs::write(p.join("c.txt"), "c\n").unwrap();
        git_in(p, &["add", "c.txt"]);
        git_in(p, &["commit", "-q", "-m", "local only"]);
        assert!(!head_is_on_upstream(p), "a local commit is not pushed");
        git_in(p, &["update-ref", "-d", "refs/remotes/origin/main"]);
        assert!(
            head_is_on_upstream(p),
            "an upstream whose tracking ref is missing cannot be ruled out"
        );
    }

    /// #1350 negative: in a shallow checkout HEAD~1 is missing but the
    /// commit has a parent; deleting the ref would drop the branch tip.
    #[test]
    fn undo_last_commit_refuses_at_a_shallow_boundary() {
        let src = TempDir::new().unwrap();
        init_repo_with_commit(src.path());
        std::fs::write(src.path().join("b.txt"), "b\n").unwrap();
        git_in(src.path(), &["add", "b.txt"]);
        git_in(src.path(), &["commit", "-q", "-m", "Add b"]);
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("shallow");
        let url = format!("file://{}", src.path().display());
        git_in(
            tmp.path(),
            &["clone", "-q", "--depth=1", &url, p.to_str().unwrap()],
        );
        let head = git_in(&p, &["rev-parse", "HEAD"]);
        let err = undo_last_commit(&p).unwrap_err();
        assert!(err.contains("shallow"), "{err}");
        assert_eq!(git_in(&p, &["rev-parse", "HEAD"]), head);
    }

    /// #1350 negative: a detached HEAD on a root commit is refused, since
    /// `update-ref -d HEAD` would delete `.git/HEAD` itself.
    #[test]
    fn undo_last_commit_refuses_a_detached_root_commit() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        git_in(p, &["checkout", "-q", "--detach"]);
        let head = git_in(p, &["rev-parse", "HEAD"]);
        let err = undo_last_commit(p).unwrap_err();
        assert!(err.contains("detached"), "{err}");
        assert!(p.join(".git/HEAD").exists());
        assert_eq!(git_in(p, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn repo_toplevel_resolves_from_a_subdirectory_and_refuses_outside_a_repo() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let sub = p.join("crates").join("foo");
        std::fs::create_dir_all(&sub).unwrap();
        let top = repo_toplevel(&sub).expect("a repo subdirectory resolves to its toplevel");
        assert_eq!(
            top.canonicalize().unwrap(),
            p.canonicalize().unwrap(),
            "the toplevel is the repo root, not the subdirectory"
        );
        let outside = TempDir::new().unwrap();
        assert_eq!(
            repo_toplevel(outside.path()),
            None,
            "outside a repo there is no toplevel"
        );
    }

    /// A new folder lists each of its files (#1279), not one `dir/` row
    /// that opens nothing and has no line count.
    #[test]
    fn a_new_folder_lists_each_of_its_files() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::create_dir_all(p.join("src/billing")).unwrap();
        std::fs::write(
            p.join("src/billing/invoice.py"),
            "def invoice(total):\n    return round(total * 1.2, 2)\n",
        )
        .unwrap();
        std::fs::write(p.join("src/billing/tax.py"), "RATE = 0.2\n").unwrap();
        let entries = query_changes(p);
        let untracked: Vec<(&str, usize)> = entries
            .iter()
            .filter(|e| e.kind == ChangeKind::Untracked)
            .map(|e| (e.path.as_str(), e.additions))
            .collect();
        assert_eq!(
            untracked,
            [("src/billing/invoice.py", 2), ("src/billing/tax.py", 1)]
        );
        assert_eq!(query(p).changed_count, 2, "the badge counts files too");
    }

    #[test]
    fn an_ignored_folder_still_lists_nothing() {
        // Negative: `-uall` lists untracked files, never ignored ones.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join(".gitignore"), "build/\n").unwrap();
        sh_git(p, &["add", ".gitignore"]);
        sh_git(p, &["commit", "-qm", "ignore"]);
        std::fs::create_dir_all(p.join("build/out")).unwrap();
        std::fs::write(p.join("build/out/a.o"), "x").unwrap();
        assert!(query_changes(p).is_empty());
        assert_eq!(query(p).changed_count, 0);
    }

    /// git's prepared message for a conflicted merge (#1282), comments
    /// dropped, and a squash's when there is no merge message.
    #[test]
    fn a_merge_in_progress_carries_gits_prepared_message() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        assert_eq!(query(p).prepared_message, None, "no merge, no message");
        std::fs::write(
            p.join(".git/SQUASH_MSG"),
            "Squashed commit of the following:\n\ncommit abc\n",
        )
        .unwrap();
        assert_eq!(
            query(p).prepared_message.as_deref(),
            Some("Squashed commit of the following:\n\ncommit abc")
        );
        std::fs::write(
            p.join(".git/MERGE_MSG"),
            "Merge branch 'other'\n\n# Conflicts:\n#\tf.txt\n",
        )
        .unwrap();
        assert_eq!(
            query(p).prepared_message.as_deref(),
            Some("Merge branch 'other'")
        );
    }

    #[test]
    fn a_prepared_message_of_only_comments_is_none() {
        // Negative: nothing left after stripping is no message.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::write(p.join(".git/MERGE_MSG"), "# Conflicts:\n#\tf.txt\n\n").unwrap();
        assert_eq!(query(p).prepared_message, None);
    }

    #[test]
    fn query_from_a_subdir_carries_the_repo_root_and_counts_untracked_lines() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let sub = p.join("crates").join("foo");
        std::fs::create_dir_all(&sub).unwrap();
        // An untracked file at the TOPLEVEL: porcelain reports it
        // toplevel-relative, so a join against the subdir workspace root
        // reads a nonexistent path and silently counts 0 lines.
        std::fs::write(p.join("notes.txt"), "one\ntwo\nthree\n").unwrap();

        let status = query(&sub);
        assert!(status.in_repo, "a subdir of a repo is inside the repo");
        assert_eq!(
            status
                .repo_root
                .as_ref()
                .expect("the status carries the discovered toplevel")
                .canonicalize()
                .unwrap(),
            p.canonicalize().unwrap(),
        );

        let entries = query_changes(&sub);
        let notes = entries
            .iter()
            .find(|e| e.path == "notes.txt")
            .expect("porcelain reports the toplevel file from a subdir workspace");
        assert_eq!(
            notes.additions, 3,
            "untracked line counts must join the porcelain path against the TOPLEVEL"
        );
    }

    /// The scrubber's history is the CURRENT BRANCH, and index 0 is HEAD.
    ///
    /// `commit_graph` logs `--branches --tags HEAD` because it draws the
    /// repo-wide graph. Feeding a scrubber from it means that with any other
    /// branch present, index 0 is whatever topological order put first —
    /// demonstrated here with a sibling branch carrying a future committer
    /// date, which sorts ahead of HEAD. Stepping back from the working tree
    /// then lands on a commit the current branch does not contain (#371).
    /// A lane's branch name and directory come from one slug, and both are
    /// safe to hand to git and to the filesystem (#348).
    #[test]
    fn a_lane_name_becomes_a_safe_branch_and_directory() {
        let plan = |name: &str| WorktreeLane::plan(Path::new("/repos/croft"), name);

        let p = plan("fix the parser").expect("an ordinary name");
        assert_eq!(p.branch, "agent/fix-the-parser");
        assert_eq!(p.path, PathBuf::from("/repos/croft-fix-the-parser"));

        // The directory is a SIBLING of the repo, never inside it: a
        // worktree nested in its own repo is a working tree git will then
        // try to track, and the Explorer would show the lane inside the
        // root it was cut from.
        assert!(
            !p.path.starts_with("/repos/croft/"),
            "a lane must not nest inside its own repo: {}",
            p.path.display()
        );

        // Shapes git refuses in a ref name, or the shell would mangle:
        // every one collapses to the separator rather than reaching git.
        for name in [
            "a..b", "a b", "a~b", "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b",
        ] {
            let p = plan(name).unwrap_or_else(|| panic!("{name} should still plan"));
            assert!(
                !p.branch.contains("..")
                    && !p.branch.contains(' ')
                    && !p.branch.contains(['~', '^', ':', '?', '*', '[', '\\']),
                "{name} produced an unsafe branch: {}",
                p.branch
            );
        }

        // A very long name is capped: the slug becomes a directory name as
        // well as a branch, and a component past ~255 bytes fails at
        // `mkdir` with "File name too long" — the user would see that
        // instead of anything about lanes.
        let long = plan(&"parser".repeat(50)).expect("a long name still plans");
        let dir = long
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(dir.len() < 100, "directory name is {} bytes", dir.len());
        assert!(!long.branch.ends_with('-'), "branch: {}", long.branch);

        // A name with nothing usable in it is REFUSED rather than silently
        // becoming `agent/` — a branch named after nothing is worse than
        // being told to pick a name.
        for empty in ["", "   ", "///", "..."] {
            assert!(plan(empty).is_none(), "{empty:?} must not yield a lane");
        }
    }

    /// A lane with uncommitted work refuses to be removed, and says so.
    ///
    /// This is the safety rule the issue names, and it is the one place
    /// where being wrong destroys work rather than merely annoying someone:
    /// `git worktree remove` on a dirty tree discards it.
    #[test]
    fn a_dirty_lane_refuses_removal_and_a_clean_one_allows_it() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        // Inside the TempDir's own parent would collide with a leftover from
        // an earlier run; a second TempDir gives this test a private
        // directory that is still a sibling of the repo.
        let side = TempDir::new().unwrap();
        let lane = side.path().join("lane-clean");

        let git = |args: &[&str]| {
            let _ = Command::new("git").args(["-C"]).arg(p).args(args).status();
        };
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane",
            lane.to_str().unwrap(),
        ]);
        assert!(lane.is_dir(), "staging: the worktree exists");

        // Clean: removal is allowed.
        assert_eq!(lane_removal_block(&lane), None, "a clean lane may go");

        // Dirty: refused, and the reason names the tree.
        std::fs::write(lane.join("scratch.txt"), "unsaved").unwrap();
        let why = lane_removal_block(&lane).expect("a dirty lane must be refused");
        assert!(
            why.contains("uncommitted"),
            "the refusal must say why: {why}"
        );

        // IGNORED files refuse too. Plain `--porcelain` omits them, so the
        // check reported this tree clean while `git worktree remove`
        // destroyed a `.env` — measured. An agent lane is exactly where an
        // uncommitted local config or an expensive `node_modules` lives.
        // The dirty file from above goes first, so this case starts clean
        // and the only thing the check can be reacting to is the ignored one.
        std::fs::remove_file(lane.join("scratch.txt")).unwrap();
        std::fs::write(lane.join(".gitignore"), "*.local\n").unwrap();
        let lane_git0 = |args: &[&str]| {
            let _ = Command::new("git")
                .args(["-C"])
                .arg(&lane)
                .args(args)
                .status();
        };
        lane_git0(&["add", "-A"]);
        lane_git0(&["commit", "-q", "-m", "ignore"]);
        std::fs::write(lane.join("secrets.local"), "TOKEN=1").unwrap();
        assert!(
            plain_status_is_clean(&lane),
            "control: plain --porcelain sees nothing, which is the trap"
        );
        let why = lane_removal_block(&lane).expect("ignored files must refuse");
        assert!(
            why.contains("ignored"),
            "the refusal must name what it is protecting: {why}"
        );
        std::fs::remove_file(lane.join("secrets.local")).unwrap();

        // Committing it makes the lane clean again.
        let lane_git = |args: &[&str]| {
            let _ = Command::new("git")
                .args(["-C"])
                .arg(&lane)
                .args(args)
                .status();
        };
        lane_git(&["add", "-A"]);
        lane_git(&["commit", "-q", "-m", "keep"]);
        assert_eq!(
            lane_removal_block(&lane),
            None,
            "a committed lane is clean again"
        );

        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["worktree", "remove", "--force", lane.to_str().unwrap()])
            .status();

        // A path git CANNOT report on is refused, not assumed clean. This is
        // the branch where being wrong destroys work: `git worktree remove`
        // discards a dirty tree, so an unreadable status must fail closed.
        // Without this case the whole not-a-worktree arm could be replaced
        // by `return None` and every other assertion here would still pass.
        let outside = TempDir::new().unwrap();
        let why = lane_removal_block(outside.path())
            .expect("a directory that is not a worktree must be refused");
        assert!(
            why.contains("worktree") || why.contains("could not run"),
            "the refusal must say what went wrong: {why}"
        );

        // And a path that does not exist at all.
        let gone = outside.path().join("no-such-lane");
        assert!(
            lane_removal_block(&gone).is_some(),
            "a missing path must be refused rather than reported clean"
        );
    }

    /// The scrubber's history is the CURRENT BRANCH, and index 0 is HEAD.
    ///
    /// `commit_graph` logs `--branches --tags HEAD` because it draws the
    /// repo-wide graph. Feeding a scrubber from it means that with any other
    /// branch present, index 0 is whatever topological order put first —
    /// demonstrated here with a sibling branch carrying a future committer
    /// date, which sorts ahead of HEAD. Stepping back from the working tree
    /// then lands on a commit the current branch does not contain (#371).
    #[test]
    fn branch_history_is_the_current_branch_with_head_first() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);

        let git = |args: &[&str]| {
            let _ = Command::new("git").args(["-C"]).arg(p).args(args).status();
        };
        // A sibling branch whose commit dates FUTURE, so any date- or
        // topo-ordered listing across all branches puts it first.
        git(&["checkout", "-q", "-b", "sibling"]);
        std::fs::write(p.join("sibling.txt"), "s").unwrap();
        git(&["add", "-A"]);
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["commit", "-q", "-m", "sibling-future"])
            .env("GIT_COMMITTER_DATE", "2038-01-01T00:00:00")
            .env("GIT_AUTHOR_DATE", "2038-01-01T00:00:00")
            .status();
        // Back to the original branch.
        git(&["checkout", "-q", "-"]);

        let head = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();

        let hist = branch_history(p, 50);
        assert!(!hist.is_empty(), "the branch has commits");
        assert_eq!(
            hist[0].hash, head,
            "index 0 must be HEAD, not whatever sorted first across branches"
        );
        assert!(
            hist.iter().all(|c| c.summary != "sibling-future"),
            "a sibling branch's commit leaked into the branch history: {:?}",
            hist.iter().map(|c| &c.summary).collect::<Vec<_>>()
        );

        // The positive control: the graph DOES include it, so the assertion
        // above is about the ref set rather than about the repo being empty.
        let graph = commit_graph(p, 50);
        assert!(
            graph.iter().any(|c| c.summary == "sibling-future"),
            "control: commit_graph should see every branch"
        );
    }

    /// #371: a file added in HEAD is absent from HEAD~1's tree and present
    /// in HEAD's — the positive control, so an absence is about the commit
    /// rather than about the listing being empty. Directories are listed,
    /// and a workspace in a repo subdirectory gets paths joined onto itself.
    #[test]
    fn tree_paths_lists_a_commits_files_and_folders_under_the_root() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        let git = |args: &[&str]| {
            let _ = Command::new("git").args(["-C"]).arg(p).args(args).status();
        };
        std::fs::create_dir_all(p.join("sub/deep")).unwrap();
        std::fs::write(p.join("sub/deep/new.txt"), "n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "add new"]);

        let before = tree_paths(p, "HEAD~1").expect("HEAD~1 lists");
        assert!(before.contains(&p.join("seed.txt")));
        assert!(!before.contains(&p.join("sub/deep/new.txt")));
        assert!(!before.contains(&p.join("sub")));

        let head = tree_paths(p, "HEAD").expect("HEAD lists");
        assert!(head.contains(&p.join("sub/deep/new.txt")));
        assert!(head.contains(&p.join("sub")), "folders are listed too");
        assert!(head.contains(&p.join("sub/deep")));

        // Opened in a subdirectory: only its entries, joined onto it.
        let sub = p.join("sub");
        let nested = tree_paths(&sub, "HEAD").expect("subdir lists");
        assert!(nested.contains(&sub.join("deep/new.txt")));
        assert!(!nested.paths.iter().any(|q| q.ends_with("seed.txt")));

        assert!(tree_paths(p, "no-such-rev").is_none());

        // A submodule's contents are never listed, so they count as present.
        let head_sha = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(p)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let cacheinfo = format!("160000,{},vendored", head_sha.trim());
        git(&["update-index", "--add", "--cacheinfo", &cacheinfo]);
        git(&["commit", "-q", "-m", "add submodule"]);
        let with_sub = tree_paths(p, "HEAD").expect("HEAD lists");
        assert_eq!(with_sub.submodules, vec![p.join("vendored")]);
        assert!(with_sub.contains(&p.join("vendored/src/lib.rs")));
        assert!(!with_sub.contains(&p.join("vendoredx")));
    }

    /// What plain `--porcelain` (no `--ignored`) reports, as the positive
    /// control for the ignored-file case: it must see NOTHING there, or the
    /// assertion that `lane_removal_block` catches it proves nothing.
    fn plain_status_is_clean(lane: &Path) -> bool {
        let out = Command::new("git")
            .args(["-C"])
            .arg(lane)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    /// A failed creation leaves the base untouched (#348).
    ///
    /// The read has to happen BEFORE the command, while HEAD is still the
    /// fork point - but writing it before the command succeeds records a
    /// base for a lane that does not exist, which a retry would then carry.
    /// Driven through the real duplicate-name refusal rather than a mocked
    /// failure, so it fails the way callers actually see it.
    #[test]
    fn a_failed_lane_creation_leaves_the_base_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        run_git_mut(p, &["checkout", "-b", "release-9"]).expect("branch");

        let mut first = WorktreeLane::plan(p, "parser work").expect("planned");
        add_worktree_lane(p, &mut first).expect("the first lane is created");
        assert_eq!(first.base.as_deref(), Some("release-9"), "precondition");

        // Same name: git refuses, so nothing was created to have a base.
        let mut second = WorktreeLane::plan(p, "parser work").expect("planned");
        assert!(add_worktree_lane(p, &mut second).is_err(), "precondition");
        assert_eq!(
            second.base, None,
            "a lane git refused to create records no base"
        );
        remove_worktree_lane(&first.path).expect("cleanup");
    }

    /// The base is recorded at creation and is not derivable afterwards
    /// (#348). Asserts against a branch the fixture NAMES, so a bug that
    /// wrote a constant, or the lane's own branch, fails here rather than
    /// looking plausible.
    #[test]
    fn a_lane_records_the_branch_it_was_cut_from() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        // A NON-default name, so passing "main"/"master" through cannot pass.
        run_git_mut(p, &["checkout", "-b", "release-42"]).expect("branch");

        let mut lane = WorktreeLane::plan(p, "parser work").expect("planned");
        assert_eq!(lane.base, None, "plan is pure and reads no git");
        add_worktree_lane(p, &mut lane).expect("git worktree add should succeed");

        assert_eq!(
            lane.base.as_deref(),
            Some("release-42"),
            "the lane records the branch HEAD was on, not a default"
        );
        assert_ne!(
            lane.base.as_deref(),
            Some(lane.branch.as_str()),
            "the base is what it forked FROM, never the lane's own branch"
        );
        remove_worktree_lane(&lane.path).expect("cleanup");
    }

    /// A lane round-trips through the real git commands (#348).
    ///
    /// Driven through `add_worktree_lane` / `remove_worktree_lane` rather
    /// than asserting on the strings they would build: the argument order
    /// and the working directory each command runs in are the parts that go
    /// wrong, and only running them can show that.
    #[test]
    fn a_lane_is_created_and_removed_through_git() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);

        let mut lane = WorktreeLane::plan(p, "parser work").expect("planned");
        assert!(lane.path.starts_with(p.parent().unwrap()), "a sibling");

        add_worktree_lane(p, &mut lane).expect("git worktree add should succeed");
        assert!(lane.path.is_dir(), "the lane directory exists");

        // The branch really was created, and the lane is on it.
        let out = Command::new("git")
            .args(["-C"])
            .arg(&lane.path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("rev-parse");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            lane.branch,
            "the lane must be on its own branch"
        );

        // A second lane of the same name is refused by git rather than
        // silently reusing the branch.
        assert!(
            add_worktree_lane(p, &mut lane).is_err(),
            "a duplicate lane must fail rather than adopt the existing one"
        );

        // A LOCKED worktree reports a clean status — `lane_removal_block`
        // asks "is there uncommitted work", which is a different question
        // from "will git remove this" — and git then refuses. Pinned
        // because it is why the caller runs git BEFORE dropping the
        // workspace folder: the other order leaves the lane gone from the
        // workspace and the directory still on disk.
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["worktree", "lock", lane.path.to_str().unwrap()])
            .status();
        assert_eq!(
            lane_removal_block(&lane.path),
            None,
            "a locked-but-clean lane has no uncommitted work"
        );
        assert!(
            remove_worktree_lane(&lane.path).is_err(),
            "git must refuse a locked worktree, and the caller must hear it"
        );
        assert!(lane.path.is_dir(), "the refusal left the lane in place");
        let _ = Command::new("git")
            .args(["-C"])
            .arg(p)
            .args(["worktree", "unlock", lane.path.to_str().unwrap()])
            .status();

        remove_worktree_lane(&lane.path).expect("a clean lane removes");
        assert!(!lane.path.exists(), "the lane directory is gone");
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
    fn a_commit_from_before_a_rename_shows_its_diff_under_the_old_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(p)
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "a@b"]);
        git(&["config", "user.name", "a"]);
        std::fs::write(p.join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "add"]);
        std::fs::write(p.join("old.txt"), "one\ntwo\nthree\nFOUR\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        let edit = git(&["rev-parse", "--short", "HEAD"]);
        git(&["mv", "old.txt", "new.txt"]);
        git(&["commit", "-qm", "rename"]);
        let diff = show_commit_file_diff(p, &edit, "new.txt").unwrap();
        assert!(diff.contains("+FOUR"), "{diff}");
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

    /// A workspace opened one folder ABOVE its repo (#1227): `tl/` holding
    /// the repo `tl/sub/`. Running git from the workspace root fails there,
    /// so history and blame must resolve from the file's own folder.
    #[test]
    fn file_history_finds_the_repo_below_the_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo_with_commit(&sub);
        let hist = file_history(tmp.path(), "sub/seed.txt", 10);
        assert_eq!(hist.len(), 1, "the commit in sub/ must be listed");
    }

    #[test]
    fn blame_finds_the_repo_below_the_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo_with_commit(&sub);
        let b = blame(tmp.path(), "sub/seed.txt");
        assert_eq!(b.len(), 2, "blame of sub/seed.txt from the parent");
        assert!(!b[0].uncommitted);
    }

    #[test]
    fn file_history_of_a_file_in_a_repo_subfolder_still_resolves() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        init_repo_with_commit(p);
        std::fs::create_dir(p.join("dir")).unwrap();
        std::fs::write(p.join("dir/x.txt"), "x\n").unwrap();
        let _ = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-qm", "x"])
            .status();
        assert_eq!(file_history(p, "dir/x.txt", 10).len(), 1);
        assert_eq!(blame(p, "dir/x.txt").len(), 1);
    }

    #[test]
    fn history_and_blame_stay_empty_outside_any_repo() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("plain.txt"), "one\n").unwrap();
        assert!(file_history(tmp.path(), "sub/plain.txt", 10).is_empty());
        assert!(blame(tmp.path(), "sub/plain.txt").is_empty());
    }

    #[test]
    fn history_of_a_file_untracked_in_a_nested_repo_is_empty() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        init_repo_with_commit(&sub);
        std::fs::write(sub.join("new.txt"), "new\n").unwrap();
        assert!(file_history(tmp.path(), "sub/new.txt", 10).is_empty());
    }

    #[test]
    fn child_repos_finds_repos_one_level_down_only() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path();
        for d in ["web", "api", "plain", ".hidden", "deep/inner"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        init_repo_with_commit(&p.join("web"));
        init_repo_with_commit(&p.join("api"));
        init_repo_with_commit(&p.join(".hidden"));
        init_repo_with_commit(&p.join("deep/inner"));
        std::fs::write(p.join("worktree_file_dir_marker"), "").unwrap();
        std::fs::create_dir(p.join("wt")).unwrap();
        std::fs::write(p.join("wt/.git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            child_repos(p),
            vec![p.join("api"), p.join("web"), p.join("wt")],
            "sorted, depth 1, hidden skipped, a .git file counts"
        );
    }

    #[test]
    fn child_repos_is_empty_for_a_missing_or_repo_free_folder() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("plain")).unwrap();
        assert!(child_repos(tmp.path()).is_empty());
        assert!(child_repos(&tmp.path().join("gone")).is_empty());
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

    /// A repo whose `*.txt` files go through a rot13 clean/smudge filter: the
    /// index and HEAD hold rot13 text, the working tree plain text, the way
    /// Git LFS keeps a pointer in git and the real file on disk (#1238).
    fn filtered_repo(root: &Path) {
        filtered_repo_with(root, "rot13");
    }

    /// [`filtered_repo`] with the driver configured under `name`.
    fn filtered_repo_with(root: &Path, name: &str) {
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?}"
            )
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        let rot13 = "tr A-Za-z N-ZA-Mn-za-m";
        run(&["config", &format!("filter.{name}.clean"), rot13]);
        run(&["config", &format!("filter.{name}.smudge"), rot13]);
        std::fs::write(
            root.join(".gitattributes"),
            format!("*.txt filter={name}\n"),
        )
        .unwrap();
        let body: String = (1..=12).map(|i| format!("row {i}\n")).collect();
        std::fs::write(root.join("data.txt"), &body).unwrap();
        std::fs::write(root.join("notes.md"), &body).unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        let edit = |name: &str| {
            let text = std::fs::read_to_string(root.join(name)).unwrap();
            std::fs::write(root.join(name), text.replace("row 5\n", "row FIVE\n")).unwrap();
        };
        edit("data.txt");
        edit("notes.md");
    }

    fn head_vs_work_patch(root: &Path, rel: &str) -> String {
        let head = read_file_at_head(root, rel).unwrap();
        let work = std::fs::read_to_string(root.join(rel)).unwrap();
        let diff = crate::widgets::diff::DiffData::build(
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            head.lines().map(str::to_string).collect(),
            work.lines().map(str::to_string).collect(),
        );
        diff.hunk_patch(rel, diff.hunk_range_at(0).unwrap())
    }

    #[test]
    fn a_filtered_files_head_side_is_what_the_filter_checks_out() {
        let tmp = TempDir::new().unwrap();
        filtered_repo(tmp.path());
        assert_eq!(
            sh_git(tmp.path(), &["show", "HEAD:data.txt"])
                .lines()
                .next(),
            Some("ebj 1"),
            "sanity: git stores the cleaned form"
        );
        let head = read_file_at_head(tmp.path(), "data.txt").unwrap();
        assert_eq!(
            head.lines().next(),
            Some("row 1"),
            "the diff compares like with like"
        );
        let patch = head_vs_work_patch(tmp.path(), "data.txt");
        assert_eq!(
            patch
                .lines()
                .filter(|l| l.starts_with(['-', '+'])
                    && !l.starts_with("---")
                    && !l.starts_with("+++"))
                .collect::<Vec<_>>(),
            vec!["-row 5", "+row FIVE"],
            "a one-line edit is a one-line change: {patch}"
        );
    }

    #[test]
    fn staging_a_hunk_of_a_filtered_file_is_refused_and_leaves_the_index() {
        let tmp = TempDir::new().unwrap();
        filtered_repo(tmp.path());
        let before = sh_git(tmp.path(), &["show", ":data.txt"]);
        let patch = head_vs_work_patch(tmp.path(), "data.txt");
        let err = apply_patch(tmp.path(), &patch, true, false).unwrap_err();
        assert_eq!(
            err,
            "data.txt uses the rot13 filter: stage the whole file, not a part"
        );
        assert_eq!(
            sh_git(tmp.path(), &["show", ":data.txt"]),
            before,
            "index untouched"
        );
        let err = apply_patch(tmp.path(), &patch, true, true).unwrap_err();
        assert!(
            err.contains("unstage the whole file"),
            "unstaging is refused too: {err}"
        );
    }

    #[test]
    fn reverting_a_hunk_of_a_filtered_file_says_why_it_cannot() {
        let tmp = TempDir::new().unwrap();
        filtered_repo(tmp.path());
        let patch = head_vs_work_patch(tmp.path(), "data.txt");
        let err = apply_patch(tmp.path(), &patch, false, true).unwrap_err();
        assert_eq!(
            err,
            "data.txt uses the rot13 filter: discard the whole file, not a part"
        );
        let work = std::fs::read_to_string(tmp.path().join("data.txt")).unwrap();
        assert!(
            work.contains("row FIVE\n"),
            "the working file is untouched: {work}"
        );
    }

    /// [`filtered_repo`] with its edits committed: HEAD~1 holds `row 5`,
    /// HEAD holds `row FIVE`, in both the filtered and the plain file.
    fn filtered_history(root: &Path) {
        filtered_repo(root);
        sh_git(root, &["commit", "-q", "-am", "Rename row 5"]);
    }

    /// The `-`/`+` lines of a unified diff, headers left out.
    fn changed_lines(diff: &str) -> Vec<&str> {
        diff.lines()
            .filter(|l| l.starts_with(['-', '+']) && !l.starts_with("---") && !l.starts_with("+++"))
            .collect()
    }

    #[test]
    fn a_filtered_files_past_versions_read_as_the_file_not_what_git_stores() {
        // #1338: Scrub History read the pointer (here, rot13 text) at every
        // past commit, and the merge editor's sides had the same gap.
        let tmp = TempDir::new().unwrap();
        filtered_history(tmp.path());
        let past = read_file_at_rev(tmp.path(), "HEAD~1", "data.txt").unwrap();
        assert!(past.starts_with("row 1\n"), "{past}");
        assert!(past.contains("row 5\n") && !past.contains("FIVE"), "{past}");
        let now = read_file_at_rev(tmp.path(), "HEAD", "data.txt").unwrap();
        assert!(now.contains("row FIVE\n"), "{now}");
        let staged = read_file_at_stage(tmp.path(), "data.txt", 0).unwrap();
        assert_eq!(staged, now, "an index stage reads through the filter too");
    }

    #[test]
    fn a_filtered_files_timeline_diff_shows_what_changed_in_the_file() {
        let tmp = TempDir::new().unwrap();
        filtered_history(tmp.path());
        let hash = sh_git(tmp.path(), &["rev-parse", "--short", "HEAD"]);
        let raw = show_commit_file_diff(tmp.path(), hash.trim(), "data.txt").unwrap();
        assert!(
            raw.starts_with("commit ") && raw.contains("    Rename row 5\n\ndiff --git"),
            "the commit header an unfiltered file's diff carries: {raw}"
        );
        assert_eq!(
            changed_lines(&raw),
            vec!["-row 5", "+row FIVE"],
            "the content change, not the stored form's: {raw}"
        );
        let view = crate::widgets::diff::DiffData::build_side_by_side_from_git_text(
            std::path::PathBuf::from("data.txt"),
            &raw,
        );
        assert!(
            view.left_lines.iter().any(|l| l == "row 5"),
            "{view:?}",
            view = view.left_lines
        );
        assert!(
            view.right_lines.iter().any(|l| l == "row FIVE"),
            "{view:?}",
            view = view.right_lines
        );

        // The commit that added the file: every line is new, nothing fails.
        let first = sh_git(tmp.path(), &["rev-parse", "--short", "HEAD~1"]);
        let raw = show_commit_file_diff(tmp.path(), first.trim(), "data.txt").unwrap();
        let lines = changed_lines(&raw);
        assert_eq!(lines.len(), 12, "{raw}");
        assert_eq!(lines[0], "+row 1");
    }

    #[test]
    fn an_unfiltered_files_history_reads_exactly_as_git_shows_it() {
        let tmp = TempDir::new().unwrap();
        filtered_history(tmp.path());
        assert_eq!(path_filter(tmp.path(), "notes.md"), None, "precondition");
        assert_eq!(
            read_file_at_rev(tmp.path(), "HEAD~1", "notes.md").unwrap(),
            sh_git(tmp.path(), &["show", "HEAD~1:notes.md"])
        );
        let hash = sh_git(tmp.path(), &["rev-parse", "--short", "HEAD"]);
        let raw = show_commit_file_diff(tmp.path(), hash.trim(), "notes.md").unwrap();
        assert_eq!(
            raw,
            sh_git(tmp.path(), &["show", hash.trim(), "--", "notes.md"]),
            "an unfiltered file keeps git's own diff, commit header and all"
        );
        // And what git stores for the filtered file is still the cleaned
        // form: only croft's reading of it changed.
        assert!(sh_git(tmp.path(), &["show", "HEAD~1:data.txt"]).starts_with("ebj 1"));
    }

    #[test]
    fn a_driver_named_like_a_check_attr_state_is_still_a_filter() {
        // `git check-attr` prints `filter: set` both for a bare `filter` and
        // for `filter=set`; the configured driver tells them apart.
        for name in ["set", "unset", "unspecified"] {
            let tmp = TempDir::new().unwrap();
            filtered_repo_with(tmp.path(), name);
            assert_eq!(path_filter(tmp.path(), "data.txt").as_deref(), Some(name));
            let head = read_file_at_head(tmp.path(), "data.txt").unwrap();
            assert_eq!(head.lines().next(), Some("row 1"), "{name}");
            let patch = head_vs_work_patch(tmp.path(), "data.txt");
            let err = apply_patch(tmp.path(), &patch, true, false).unwrap_err();
            assert!(err.contains(&format!("the {name} filter")), "{err}");
        }
    }

    #[test]
    fn a_bare_or_unset_filter_attribute_names_no_filter() {
        // Negative: with no driver behind them these are not filters.
        let tmp = TempDir::new().unwrap();
        filtered_repo(tmp.path());
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.txt filter=rot13\nbare.md filter\noff.md -filter\n",
        )
        .unwrap();
        assert_eq!(path_filter(tmp.path(), "bare.md"), None);
        assert_eq!(path_filter(tmp.path(), "off.md"), None);
        assert_eq!(path_filter(tmp.path(), "notes.md"), None);
        assert_eq!(
            path_filter(tmp.path(), "data.txt").as_deref(),
            Some("rot13")
        );
    }

    #[test]
    fn an_unfiltered_file_beside_a_filtered_one_still_stages_a_hunk() {
        let tmp = TempDir::new().unwrap();
        filtered_repo(tmp.path());
        let patch = head_vs_work_patch(tmp.path(), "notes.md");
        apply_patch(tmp.path(), &patch, true, false).unwrap();
        assert!(sh_git(tmp.path(), &["diff", "--cached"]).contains("+row FIVE"));
    }
}
