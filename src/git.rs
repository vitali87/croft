use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

mod changes;
mod contents;
mod operations;
mod patches;
mod status;
pub use changes::ChangeEntry;
pub use changes::ChangeKind;
pub use changes::ChangeSection;
pub use changes::GitDecoration;
pub use changes::query_changes;
pub use changes::unquote_porcelain_path;
use changes::*;
pub use contents::read_bytes_at_head;
pub use contents::read_file_at_head;
pub use contents::read_file_at_rev;
pub use contents::read_file_at_stage;
pub use contents::remerge_diff3;
use contents::*;
pub use operations::RepoOp;
pub use operations::abort_operation;
pub use operations::bisect_mark;
pub use operations::continue_operation;
pub use operations::repo_operation;
pub use operations::skip_operation;
use operations::*;
pub use patches::apply_patch;
pub use status::GitStatus;
pub use status::child_repos;
pub use status::is_ignored;
pub use status::query;
use status::*;

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
    run_mutation_with(root, args, false)
}

/// [`run_mutation`] for an operation that can stop on conflicts: merge,
/// rebase, pull, stash apply and pop. Git names the conflicts on stdout
/// (`CONFLICT (content): Merge conflict in pricing.py`) and, for a rebase,
/// the failure on stderr (`error: could not apply …`); keeping stderr alone
/// lost the conflicts (#858), so a failure here carries both, stdout first.
fn run_mutation_keeping_conflicts(root: &Path, args: &[&str]) -> Result<String, String> {
    run_mutation_with(root, args, true)
}

/// [`run_mutation`]'s body; `both_streams` makes a failure report stdout
/// and stderr together when git wrote to both.
fn run_mutation_with(root: &Path, args: &[&str], both_streams: bool) -> Result<String, String> {
    run_mutation_inner(root, args, &[], both_streams)
}

/// [`run_mutation`] with extra environment variables for git.
fn run_mutation_with_env(
    root: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String, String> {
    run_mutation_inner(root, args, envs, false)
}

/// The body shared by [`run_mutation_with`] and [`run_mutation_with_env`].
fn run_mutation_inner(
    root: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    both_streams: bool,
) -> Result<String, String> {
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
    cmd.envs(envs.iter().copied());
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
        let msg = if both_streams && !stdout.is_empty() && !stderr.is_empty() {
            // Appended to stdout's own buffer rather than formatted into a
            // new one, however long git's text is.
            let mut both = stdout;
            both.reserve(1 + stderr.len());
            both.push('\n');
            both.push_str(&stderr);
            both
        } else if !stderr.is_empty() {
            stderr
        } else {
            stdout
        };
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

/// Commit the way the Commit button, Enter in the message box, Commit &
/// Push and Commit & Sync do, by VS Code's rule (#965): with anything
/// staged, only the index (`git commit -m`), so hunks staged by hand go in
/// and the rest stays in the working tree; with nothing staged, every
/// tracked change (`git commit -am`, VS Code's smart commit). The index is
/// read at commit time, not from the panel's last status poll. Refuses an
/// empty message and an unresolved merge ([`refuse_unmerged`]); on failure
/// returns git's message verbatim so the user sees exactly what blocked them.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".to_string());
    }
    refuse_unmerged(root)?;
    let flags = if has_staged_changes(root)? {
        "-m"
    } else {
        "-am"
    };
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    // A hook or signing helper that wants a terminal fails fast with its
    // own message instead of fighting croft for the screen (#1153).
    let mut cmd = Command::new("git");
    cmd.args(["-C", path_str, "commit", flags, message]);
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

/// The note git prints for each file a merge, rebase or stash pop merged
/// (`Auto-merging pricing.py`), before the line that says how that went.
const AUTO_MERGING: &str = "Auto-merging ";

/// What git's conflict line says before the file (`CONFLICT (content):
/// Merge conflict in pricing.py`).
const MERGE_CONFLICT_IN: &str = "Merge conflict in ";

/// `line` as a terminal shows it. Git redraws a progress line (`Rebasing
/// (1/1)`) by returning the carriage and, on a capable terminal, erasing
/// the line (`ESC [K`) before the next text, so what is left on screen is
/// the last redraw that says something (#858).
fn shown(line: &str) -> &str {
    line.rsplit('\r')
        .map(|part| part.trim_start_matches("\u{1b}[K"))
        .find(|part| !part.trim().is_empty())
        .unwrap_or_default()
}

/// `line` with its runs of whitespace collapsed to one space.
fn squash(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of git's `output` (for a commit, `[branch sha] subject`):
/// what the Source Control panel and the status bar report. The rest, a
/// commit's diffstat and one `create mode` row per new file, ran on past
/// both (#858); the Git Output log keeps it whole. A merge's `Auto-merging`
/// notes give way to the result after them ("Merge made by the 'ort'
/// strategy."), and a line git redrew reads as [`shown`] ("Successfully
/// rebased …", not "Rebasing (1/1)"); a text of nothing but those notes is
/// its first one, never an empty line.
pub fn headline(output: &str) -> &str {
    let said = || {
        output
            .lines()
            .map(shown)
            .filter(|line| !line.trim().is_empty())
    };
    said()
        .find(|line| !line.starts_with(AUTO_MERGING))
        .or_else(|| said().next())
        .unwrap_or_default()
        .trim_end()
}

/// The one line of git's `output` the status bar and the Source Control
/// panel report (#858); the Git Output log keeps the whole text. For a push
/// that is its ref update (`5a6cd5c..9f1e2d3 main -> main`, `[new branch]
/// main -> main`, `[rejected] main -> main (fetch first)`), otherwise the
/// first line that is not `remote:` chatter, a `hint:`, the `To <url>`
/// destination, the `On branch` / `Your branch` preamble git prints before
/// "nothing to commit" or an `Auto-merging` note. Lines read as [`shown`],
/// and runs of spaces collapse to one. Empty when nothing is left, as for a
/// push that printed only `remote:` lines.
pub fn summary_line(output: &str) -> String {
    const NOISE: [&str; 6] = [
        "remote:",
        "hint:",
        "To ",
        "On branch ",
        "Your branch ",
        AUTO_MERGING,
    ];
    let lines = || {
        output
            .lines()
            .map(|l| shown(l).trim())
            .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.starts_with(n)))
    };
    let Some(line) = lines()
        .find(|l| l.contains(" -> "))
        .or_else(|| lines().next())
    else {
        return String::new();
    };
    let mut words = line.split_whitespace().peekable();
    // A ref update leads with git's one-character flag (`*`, `+`, `-`, `!`,
    // `=`, or a space, which trimming already took).
    if words
        .peek()
        .is_some_and(|w| matches!(*w, "*" | "+" | "-" | "!" | "="))
    {
        words.next();
    }
    words.collect::<Vec<_>>().join(" ")
}

/// The one line a git operation that succeeded reports in the status bar
/// and the Source Control panel (#858): git's result ("Already up to
/// date.", "Fast-forward", "Merge made by the 'ort' strategy.",
/// "Successfully rebased and updated refs/heads/main.", "Dropped
/// refs/stash@{0} (…)", "[main 5a6cd5c] subject"). Passed over on the way:
/// the fetch's `From <url>` and the push's `To <url>`, `remote:` chatter,
/// `hint:`s, `Updating a..b`, `Fetching <remote>`, `Auto-merging` notes,
/// the `warning:` / `error:` / `fatal:` asides a success can still carry
/// (a failed push negotiation), the `git status` dump a stash or a switch
/// prints, the `M\tpath` rows a switch lists, and every indented line (ref
/// updates, the diffstat, file lists). Lines read as [`shown`]. When
/// nothing else is left, the ref update if there is one ([`summary_line`]),
/// else empty, and the caller's own word ("Pulled", "Applied stash") says
/// it; the Git Output log keeps git's whole text.
pub fn result_line(output: &str) -> String {
    const NOISE: [&str; 18] = [
        "From ",
        "To ",
        "remote:",
        "hint:",
        "Updating ",
        "Fetching ",
        AUTO_MERGING,
        "warning:",
        "error:",
        "fatal:",
        "On branch ",
        "Your branch ",
        "Changes not staged for commit:",
        "Changes to be committed:",
        "Untracked files:",
        "Unmerged paths:",
        "no changes added to commit",
        "nothing added to commit",
    ];
    // `M\tpath`: a file `git switch` carried over.
    let file_row = |line: &str| {
        let bytes = line.as_bytes();
        bytes.len() > 1 && bytes[0].is_ascii_uppercase() && bytes[1] == b'\t'
    };
    let result = output.lines().map(shown).find(|line| {
        !line.trim().is_empty()
            && !line.starts_with(char::is_whitespace)
            && !file_row(line)
            && !NOISE.iter().any(|n| line.starts_with(n))
    });
    match result {
        Some(line) => line.trim_end().to_string(),
        None if output.contains(" -> ") => summary_line(output),
        None => String::new(),
    }
}

/// The conflicts git stopped on, from the `CONFLICT …` lines of its
/// `output` (#858): "1 conflict (pricing.py)", "2 conflicts (a.py, b.py)",
/// naming three files at most before "…". A lone conflict git words
/// without "Merge conflict in <file>" (`CONFLICT (modify/delete): …`) is
/// git's own line. `None` when git reported no conflict.
pub fn conflict_summary(output: &str) -> Option<String> {
    const NAMED: usize = 3;
    let conflicts: Vec<&str> = output
        .lines()
        .map(|l| shown(l).trim())
        .filter(|l| l.starts_with("CONFLICT ("))
        .collect();
    match conflicts.as_slice() {
        [] => return None,
        [only] if !only.contains(MERGE_CONFLICT_IN) => return Some(squash(only)),
        _ => {}
    }
    let mut files: Vec<&str> = Vec::new();
    let mut unnamed = 0;
    for line in &conflicts {
        match line.split_once(MERGE_CONFLICT_IN) {
            Some((_, file)) if !files.contains(&file.trim()) => files.push(file.trim()),
            Some(_) => {}
            None => unnamed += 1,
        }
    }
    let count = files.len() + unnamed;
    let noun = if count == 1 { "conflict" } else { "conflicts" };
    if files.is_empty() {
        return Some(format!("{count} {noun}"));
    }
    let mut names = files[..files.len().min(NAMED)].join(", ");
    if count > NAMED.min(files.len()) {
        names.push_str(", \u{2026}");
    }
    Some(format!("{count} {noun} ({names})"))
}

/// The one line of git's error text `err` the status bar and the Source
/// Control panel report (#858); the Git Output log keeps the whole text.
/// First the conflicts git stopped on ([`conflict_summary`]), then a
/// rejected ref update (`[rejected] main -> main (fetch first)`), then
/// git's first `error:` or `fatal:` line, with the files git listed under
/// it ([`with_listed_files`]), then [`summary_line`]. Failing all of those,
/// the first line, so a refusal always says something.
pub fn error_line(err: &str) -> String {
    if let Some(conflicts) = conflict_summary(err) {
        return conflicts;
    }
    let lines = || err.lines().map(|l| shown(l).trim());
    if let Some(rejected) = lines().find_map(|l| l.strip_prefix('!').filter(|_| l.contains(" -> ")))
    {
        return squash(rejected);
    }
    if let Some(at) = lines().position(|l| l.starts_with("error:") || l.starts_with("fatal:")) {
        let line = lines().nth(at).map(squash).unwrap_or_default();
        return with_listed_files(line, err.lines().skip(at + 1));
    }
    let line = summary_line(err);
    if line.is_empty() {
        headline(err).to_string()
    } else {
        line
    }
}

/// `line`, git's `error:` line, followed by the files git listed under it
/// when it ends in a colon (#925): "…would be overwritten by checkout:
/// a.txt" names what is in the way, where the line alone stopped at the
/// colon. Git indents each file on a line of its own after it; three are
/// named at most before "…".
fn with_listed_files<'a>(mut line: String, after: impl Iterator<Item = &'a str>) -> String {
    const NAMED: usize = 3;
    if !line.ends_with(':') {
        return line;
    }
    let files: Vec<&str> = after
        .map(shown)
        .take_while(|l| l.starts_with(char::is_whitespace) && !l.trim().is_empty())
        .map(str::trim)
        .collect();
    if files.is_empty() {
        return line;
    }
    line.push(' ');
    line.push_str(&files[..files.len().min(NAMED)].join(", "));
    if files.len() > NAMED {
        line.push_str(", \u{2026}");
    }
    line
}

/// Whether git refused a branch switch because it would overwrite local
/// changes (#925), the refusal a stash gets past. Git ends it with "Please
/// commit your changes or stash them before you switch branches."; a switch
/// blocked by untracked files asks to "move or remove them" instead, and
/// when git names both a stash of the tracked changes still leaves the
/// switch refused, so that is not one either.
pub fn local_changes_block_switch(err: &str) -> bool {
    err.contains("commit your changes or stash them") && !err.contains("move or remove them")
}

/// A failed git operation's one line (#858), `noun` naming the operation
/// ("Merge", "Pop stash"): "Merge: 1 conflict (pricing.py)" when git
/// stopped on conflicts, which wait on the user to resolve them, otherwise
/// "Merge failed: " and git's [`error_line`].
pub fn failure_line(noun: &str, err: &str) -> String {
    match conflict_summary(err) {
        Some(conflicts) => format!("{noun}: {conflicts}"),
        None => format!("{noun} failed: {}", error_line(err)),
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

/// The commit HEAD and `branch` last had in common (`git merge-base`), or
/// None when they share no history or `branch` does not resolve.
pub fn merge_base(root: &Path, branch: &str) -> Option<String> {
    diff_text(root, &["merge-base", "HEAD", branch])
        .ok()
        .map(|out| out.trim().to_string())
        .filter(|sha| !sha.is_empty())
}

/// The working tree against the point the current branch left `branch`
/// (its merge base), as a pull request diff shows it: every committed and
/// uncommitted change on this branch, and nothing `branch` gained since
/// (#1444). Diffing against `branch`'s tip listed that later work as this
/// branch deleting or reverting it. With no shared history it falls back
/// to the tip. Used by the Source Control dropdown's "View Changes vs
/// <default>" and that tab's refresh.
pub fn diff_against_branch(root: &Path, branch: &str) -> Result<String, String> {
    match merge_base(root, branch) {
        Some(base) => diff_text(root, &["diff", &base]),
        None => diff_text(root, &["diff", branch]),
    }
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
    cmd.args(["-C", path_str]);
    match args.split_first() {
        // The patch croft parses, never the user's display settings
        // (#1447): `color.ui = always` wrapped every line in escape codes,
        // and a `diff.external` tool (difftastic) printed its own view in
        // place of the patch.
        Some((sub, rest)) if matches!(*sub, "diff" | "show") => {
            cmd.arg(sub)
                .args(["--no-color", "--no-ext-diff"])
                .args(rest);
        }
        _ => {
            cmd.args(args);
        }
    }
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

/// The top of the repository that holds `rel_path`, found from the file's
/// own folder as [`file_cwd`] finds it, and the file's path from that top,
/// which is how the rename walk in [`path_at_commit`] names it. Git run in a
/// workspace that sits above a nested repository finds none (#1556). Falls
/// back to `root` and `rel_path` outside any repository.
fn repo_top_and_path(root: &Path, rel_path: &str) -> (PathBuf, String) {
    let (dir, name) = file_cwd(root, rel_path);
    let out = run_git(&dir, &["rev-parse", "--show-toplevel", "--show-prefix"]);
    let mut lines = out.as_deref().unwrap_or_default().lines();
    match lines.next() {
        Some(top) if !top.is_empty() => {
            let prefix = lines.next().unwrap_or_default();
            (PathBuf::from(top), format!("{prefix}{name}"))
        }
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
/// for opening a TIMELINE entry in the side-by-side diff viewer. Run in the
/// repository that holds the file, wherever the workspace `root` sits.
pub fn show_commit_file_diff(root: &Path, hash: &str, rel_path: &str) -> Result<String, String> {
    let (top, path) = repo_top_and_path(root, rel_path);
    let (root, rel_path) = (top.as_path(), path.as_str());
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

/// The commits that touched `rel_path`, newest first, each with the name
/// the file had there: `git log --follow`, so the walk carries on past a
/// rename under the old name (#1304). Paths are relative to `root`, as
/// [`read_file_at_rev`] takes them. Empty when git fails.
pub fn follow_names(root: &Path, rel_path: &str, limit: usize) -> Vec<(String, String)> {
    let Ok(out) = run_git(
        root,
        &[
            "log",
            "--follow",
            // The scrubber walks first parents: a rename merged from a side
            // branch then lands on the merge, which a full walk skips for
            // the side branch's own commits (#1304).
            "--first-parent",
            &format!("-n{limit}"),
            "--name-only",
            "--relative",
            "--format=%x1e%H",
            "--",
            rel_path,
        ],
    ) else {
        return Vec::new();
    };
    out.split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let hash = lines.next()?.trim();
            let path = lines.map(str::trim).find(|l| !l.is_empty())?;
            (!hash.is_empty()).then(|| (hash.to_string(), unquote_porcelain_path(path)))
        })
        .collect()
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
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", path_str, "blame", "--porcelain", "--", &name])
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
    refuse_conflict_markers(root, Some(rel_paths))?;
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

/// Whether `rel_path` is a submodule: a gitlink (mode `160000`) in the
/// index or in HEAD (#1235). Porcelain status lists one as a plain path, so
/// a Changes row cannot tell on its own, and a gitlink has no blob for
/// `git show HEAD:<path>` and nothing for `git checkout -- <path>` to do.
pub fn is_submodule(root: &Path, rel_path: &str) -> bool {
    let gitlink = |args: &[&str]| {
        run_git(root, args).is_ok_and(|out| out.lines().any(|l| l.starts_with("160000 ")))
    };
    gitlink(&["--literal-pathspecs", "ls-files", "--stage", "--", rel_path])
        || gitlink(&["--literal-pathspecs", "ls-tree", "HEAD", "--", rel_path])
}

/// What changed in submodule `rel_path` since HEAD, as `git diff` reports a
/// submodule: the commits it moved across (`--submodule=log`), then its
/// own changes, staged or not (`--submodule=diff`), with paths inside it.
pub fn submodule_diff(root: &Path, rel_path: &str) -> Result<String, String> {
    let diff = |mode: &str| {
        run_git(
            root,
            &[
                "-c",
                "color.ui=never",
                "--literal-pathspecs",
                "diff",
                mode,
                "HEAD",
                "--",
                rel_path,
            ],
        )
        .map_err(|e| e.to_string())
    };
    let log = diff("--submodule=log")?;
    let content: String = diff("--submodule=diff")?
        .lines()
        .filter(|l| !l.starts_with("Submodule "))
        .map(|l| format!("{l}\n"))
        .collect();
    Ok(if content.is_empty() {
        log
    } else {
        format!("{log}\n{content}")
    })
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
        // `git checkout -- <submodule>` exits 0 and changes nothing, which
        // reported "Discarded" over a submodule still on its new commit
        // with its edits intact (#1235). Say so instead.
        if is_submodule(root, rel_path) {
            return Err(format!(
                "{rel_path} is a submodule; open it to discard its changes"
            ));
        }
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
    run_mutation_keeping_conflicts(root, &["pull"])
}

/// A single branch the Checkout/Create picker can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// What the user sees: the local branch name, or `origin/foo` for a
    /// remote-tracking branch.
    pub display: String,
    /// The branch's own name: the local name, or a remote branch's name
    /// without its remote (`feat` for `upstream/feat`). What merge, rebase,
    /// delete and create-from act on.
    pub checkout_name: String,
    /// True for the branch currently checked out (marked in the picker,
    /// never offered as a checkout target).
    pub is_current: bool,
    /// True for `origin/…` remote-tracking branches, listed below locals.
    pub is_remote: bool,
    /// For a remote-tracking branch, the ref to check out with `git switch
    /// --track` (`upstream/feat`), so the local branch tracks the remote
    /// the user picked. `git switch feat` refuses a name two remotes carry
    /// (#1445). `None` for a local branch.
    pub track: Option<String>,
}

/// List branches for the Checkout picker: local branches first (most
/// recently committed on top, the current one flagged), then remote-only
/// branches whose name has no local counterpart. Remote rows carry their
/// remote ref in `track`, so selecting `upstream/foo` runs `git switch
/// --track upstream/foo` and the new local branch tracks that remote.
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
            track: None,
        });
    }
    let remotes_raw = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(symref)",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    // Longest first, so `my/fork/x` splits at the `my/fork` remote and not
    // at a `my` one; splitting at the first `/` broke a remote named with
    // one (#1445).
    let mut remotes: Vec<String> = run_git(root, &["remote"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    remotes.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for line in remotes_raw.lines() {
        let (refname, symref) = line.split_once('\t').unwrap_or((line, ""));
        // `refs/remotes/origin/HEAD` is a pointer to a branch, not one: git
        // shortens it to plain `origin`, which the old `/HEAD` check missed.
        let Some(full) = refname.strip_prefix("refs/remotes/") else {
            continue;
        };
        if full.is_empty() || !symref.is_empty() || full.ends_with("/HEAD") {
            continue;
        }
        let short = remotes
            .iter()
            .find_map(|r| full.strip_prefix(r.as_str())?.strip_prefix('/'))
            .or_else(|| full.split_once('/').map(|(_, s)| s))
            .unwrap_or(full);
        if local_names.contains(short) {
            continue;
        }
        branches.push(BranchInfo {
            display: full.to_string(),
            checkout_name: short.to_string(),
            is_current: false,
            is_remote: true,
            track: Some(full.to_string()),
        });
    }
    Ok(branches)
}

/// Git's messages in English, whatever the user's locale (#925): a
/// refusal to switch is told apart by its text
/// ([`local_changes_block_switch`]), which a translated git words otherwise.
/// `LANGUAGE` picks gettext's catalog ahead of `LC_ALL` and `LANG` and
/// leaves the character set alone; git has no `en` catalog, so its own
/// text is what prints.
const ENGLISH_MESSAGES: (&str, &str) = ("LANGUAGE", "en");

/// Switch to an existing local branch (`git switch <name>`).
pub fn checkout_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation_with_env(root, &["switch", name], &[ENGLISH_MESSAGES])
}

/// Check out a remote-tracking branch as local branch `name` tracking it
/// (`git switch -c feat --track upstream/feat`): the remote the user
/// picked, even when another remote has a branch of the same name (#1445).
/// The name is spelled out because `--track` alone guesses it by cutting
/// at the first `/`, which makes `my/fork/solo` a `fork/solo` branch.
pub fn track_remote_branch(root: &Path, remote_ref: &str, name: &str) -> Result<String, String> {
    run_mutation_with_env(
        root,
        &["switch", "-c", name, "--track", remote_ref],
        &[ENGLISH_MESSAGES],
    )
}

/// Create a new branch off the current HEAD and switch to it
/// (`git switch -c <name>`). Errors (e.g. name already exists) carry
/// git's verbatim message.
pub fn create_branch(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "-c", name])
}

/// Stash the working tree (`git stash push`). Tracked changes are saved
/// and the working tree reverts to HEAD, exactly like VS Code's Stash.
/// A `message` names the stash (`-m`, #1581); without one git calls it
/// `WIP on <branch>: <commit>`.
pub fn stash_push(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &[], message)
}

/// Which of Source Control's three stash commands is asking for a
/// message (#1581).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StashKind {
    /// `git stash push`.
    Tracked,
    /// `git stash push -u`.
    IncludeUntracked,
    /// `git stash push --staged`.
    Staged,
}

/// `git stash push <extra> [-m <message>]`, a blank message counting as
/// none.
fn stash_push_args(root: &Path, extra: &[&str], message: Option<&str>) -> Result<String, String> {
    let mut args = vec!["stash", "push"];
    args.extend_from_slice(extra);
    if let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) {
        args.extend(["-m", message]);
    }
    run_mutation(root, &args)
}

/// Restore and drop the most recent stash (`git stash pop`). A pop
/// conflict surfaces git's verbatim message so the user can resolve it.
pub fn stash_pop(root: &Path) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "pop"])
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

/// Paths git still holds as unmerged (`UU`, `AA`, `DU`, …): the MERGE
/// CONFLICTS group, read straight from the index.
pub fn unmerged_paths(root: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        // `--no-relative`: with `diff.relative` set, a subdirectory root
        // would hide the conflicts outside it, and `git add -A` stages them.
        .args(["diff", "--no-relative", "--name-only", "--diff-filter=U", "-z"])
        .output()
    else {
        return Vec::new();
    };
    let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    paths.dedup();
    paths
}

/// Refuse to commit while a merge still has unresolved conflicts (#989).
/// `git commit -a` and `git add -A` stage an unmerged file as it is, so the
/// commit would record its `<<<<<<<` markers; a plain `git commit` refuses
/// here by itself, and this says the same thing up front for the paths that
/// stage on the user's behalf.
pub fn refuse_unmerged(root: &Path) -> Result<(), String> {
    let paths = unmerged_paths(root);
    if paths.is_empty() {
        return Ok(());
    }
    let named = first_paths(&paths);
    let (n, them) = match paths.len() {
        1 => (String::from("1 merge conflict"), "it"),
        n => (format!("{n} merge conflicts"), "them"),
    };
    Err(format!(
        "Resolve {n} ({named}) and stage {them} before committing"
    ))
}

/// Whether the index holds anything a plain `git commit` would record: a
/// change against HEAD, or on an unborn branch any staged file.
fn has_staged_changes(root: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // `--no-relative`: with `diff.relative` set, a subdirectory root
        // would see only what is staged under it, and a commit takes it all.
        // `--ignore-submodules=none`: `diff.ignoreSubmodules` hides a staged
        // submodule update from diff, but `git commit` still records it.
        // `--no-ext-diff`: an external diff helper whose exit code is trusted
        // (`diff.trustExitCode`, git 2.46+) could call a staged change equal.
        .args([
            "diff",
            "--cached",
            "--quiet",
            "--no-relative",
            "--ignore-submodules=none",
            "--no-ext-diff",
        ])
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        code => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("git diff --cached failed with code {code:?}")
            } else {
                stderr
            })
        }
    }
}

/// Up to three of `paths`, comma-separated, with `…` when there are more.
fn first_paths(paths: &[String]) -> String {
    let mut named = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 3 {
        named.push_str(", …");
    }
    named
}

/// Refuse to stage a conflicted file that still holds its conflict markers
/// (#989). `git add` is how git marks a conflict resolved, and it takes the
/// file as it is, so Stage All, or staging the file itself, would record
/// the `<<<<<<<` lines and the next commit would ship them. A conflicted
/// file whose markers have been edited away stages as usual. `scope` holds
/// the `root`-relative paths about to be staged; `None` means everything.
/// The markers are found by [`leftover_conflict_markers`].
fn refuse_conflict_markers(root: &Path, scope: Option<&[String]>) -> Result<(), String> {
    let unmerged = unmerged_paths(root);
    if unmerged.is_empty() {
        return Ok(());
    }
    // `unmerged_paths` names paths from the top of the repository, which
    // is not `root` when croft was opened on a subdirectory.
    let Some(top) = repo_toplevel(root) else {
        return Ok(());
    };
    let canon = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let scope: Option<Vec<PathBuf>> =
        scope.map(|paths| paths.iter().map(|p| canon(root.join(p))).collect());
    let marked: Vec<String> = unmerged
        .into_iter()
        .filter(|rel| {
            let abs = canon(top.join(rel));
            scope
                .as_ref()
                .is_none_or(|scope| scope.iter().any(|s| abs.starts_with(s)))
                && leftover_conflict_markers(&top, rel)
        })
        .collect();
    if marked.is_empty() {
        return Ok(());
    }
    let (has, them) = if marked.len() == 1 {
        ("has", "it")
    } else {
        ("have", "them")
    };
    Err(format!(
        "{} still {has} conflict markers: resolve {them} before staging",
        first_paths(&marked)
    ))
}

/// Whether git's own `diff --check` finds conflict markers in the unmerged
/// `rel` (a path from the top of the repository at `top`). Reading the
/// file ourselves missed three cases git handles: a file in Latin-1 or
/// Shift_JIS, markers of another length set by a `conflict-marker-size`
/// attribute, and marker lines both sides already had, such as a doc
/// showing what a conflict looks like, which would block staging forever.
fn leftover_conflict_markers(top: &Path, rel: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["--literal-pathspecs", "diff", "--check", "--no-color", "--"])
        .arg(rel)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("leftover conflict marker"))
}

/// Commit only what is already staged (`git commit -m`), leaving unstaged
/// changes in the working tree. With nothing staged git refuses it, where
/// [`commit`] would take every tracked change.
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

/// Stage every change, tracked and untracked (`git add -A`). Refused while
/// a conflicted file still holds its markers.
pub fn stage_all(root: &Path) -> Result<String, String> {
    refuse_conflict_markers(root, None)?;
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
    run_mutation_keeping_conflicts(root, &["pull", "--rebase"])
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
    match default_push_remote(root)? {
        Some(remote) => Ok(remote),
        None if run_git(root, &["remote"]).is_ok_and(|r| r.is_empty()) => {
            Err(String::from("no remote to publish this branch to"))
        }
        None => Err(String::from(
            "branch has no upstream and several remotes, none of them origin: set remote.pushDefault to pick one",
        )),
    }
}

/// `remote.pushDefault`, else the only remote, else `origin` among
/// several; `None` when there is no remote or several and none of those.
fn default_push_remote(root: &Path) -> Result<Option<String>, String> {
    if let Ok(remote) = run_git(root, &["config", "--get", "remote.pushDefault"])
        && !remote.is_empty()
    {
        return Ok(Some(remote));
    }
    let listed = run_git(root, &["remote"]).map_err(|e| e.to_string())?;
    let remotes: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    Ok(match remotes.as_slice() {
        [only] => Some((*only).to_string()),
        several if several.contains(&"origin") => Some(String::from("origin")),
        _ => None,
    })
}

/// The remote tags are pushed to and deleted from (#1582): the one a new
/// branch is published to, by the same rules.
pub fn tag_remote(root: &Path) -> Result<String, String> {
    match default_push_remote(root)? {
        Some(remote) => Ok(remote),
        None if run_git(root, &["remote"]).is_ok_and(|r| r.is_empty()) => {
            Err(String::from("no remote to push tags to"))
        }
        None => Err(String::from(
            "several remotes, none of them origin: set remote.pushDefault to pick one",
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
    run_mutation_keeping_conflicts(root, &["merge", branch])
}

/// Rebase the current branch onto `branch` (`git rebase <branch>`).
pub fn rebase_branch(root: &Path, branch: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["rebase", branch])
}

// --- Commit actions (the COMMITS graph's right-click menu, #1627) ---------

/// Apply `hash` on top of HEAD (`git cherry-pick <hash>`). A conflict
/// leaves `CHERRY_PICK_HEAD`, so Git: Continue / Skip / Abort take over.
pub fn cherry_pick(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["cherry-pick", hash])
}

/// Commit the inverse of `hash` with git's own "Revert ..." message
/// (`git revert --no-edit <hash>`); a conflict leaves `REVERT_HEAD`.
pub fn revert_commit(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["revert", "--no-edit", hash])
}

/// Check out `hash` with no branch (`git switch --detach <hash>`).
pub fn checkout_detached(root: &Path, hash: &str) -> Result<String, String> {
    run_mutation(root, &["switch", "--detach", hash])
}

/// Create a lightweight tag at `target` (`git tag <name> <target>`).
pub fn create_tag_at(root: &Path, name: &str, target: &str) -> Result<String, String> {
    run_mutation(root, &["tag", name, target])
}

/// How far [`reset_to`] rewinds: HEAD only, HEAD and the index, or HEAD,
/// the index and the working tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

impl ResetMode {
    fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        }
    }
}

/// Move the current branch to `hash` (`git reset --soft|--mixed|--hard`).
pub fn reset_to(root: &Path, hash: &str, mode: ResetMode) -> Result<String, String> {
    run_mutation(root, &["reset", "-q", mode.flag(), hash])
}

/// A snapshot of what a hard reset would throw away: HEAD and the branch
/// it is on (empty when detached), the index (each entry's blob, so
/// staging another hunk of an already-staged file changes it), the status
/// of every changed or untracked path, and each such file's size and
/// mtime, so a save to an already-modified file changes it too. A warned
/// Reset --hard (#1627) is confirmed only while this is unchanged.
pub fn worktree_fingerprint(root: &Path) -> String {
    let head = run_git(root, &["rev-parse", "HEAD"]).unwrap_or_default();
    let branch = run_git(root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let index = run_git(root, &["ls-files", "-s", "-z"]).unwrap_or_default();
    // Not `run_git`: its trim would eat the first entry's leading status
    // space (" M path"), and with it where that path starts.
    let status = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let top = repo_toplevel(root).unwrap_or_else(|| root.to_path_buf());
    let mut out = format!("{head}\n{branch}\n{index}\n");
    for entry in status.split('\0').filter(|e| !e.is_empty()) {
        out.push_str(entry);
        let path = entry.get(3..).unwrap_or(entry);
        if let Ok(meta) = std::fs::metadata(top.join(path)) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.push_str(&format!(" {} {mtime}", meta.len()));
        }
        out.push('\n');
    }
    out
}

/// The full message of `hash` (`git log -1 --format=%B`), trimmed.
pub fn commit_message(root: &Path, hash: &str) -> Result<String, String> {
    run_git(root, &["log", "-1", "--format=%B", hash])
        .map(|m| m.trim().to_string())
        .map_err(|e| e.to_string())
}

/// The inline blame's hover card for `hash` (#1556): author and email, the
/// full date, the short hash and the whole message, which `git blame`
/// does not give (its `summary` is the first line). `--no-show-signature`
/// because a `log.showSignature = true` config would otherwise print gpg's
/// verdict above the card.
pub fn commit_card(dir: &Path, hash: &str) -> Result<String, String> {
    let format = "--format=%an <%ae>%n%ad (%ar)%ncommit %h%n%n%B";
    run_git(dir, &["log", "-1", "--no-show-signature", format, hash]).map_err(|e| e.to_string())
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
pub fn stash_push_untracked(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &["-u"], message)
}

/// Stash only the staged changes (`git stash push --staged`).
pub fn stash_push_staged(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &["--staged"], message)
}

/// One entry in the stash list, for the apply/pop/drop pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashInfo {
    /// Stash index (`stash@{N}` is built from this).
    pub index: usize,
    /// The human description git stores (`WIP on main: …`).
    pub message: String,
    /// How long ago it was made (`5 minutes ago`, #1581).
    pub age: String,
    /// The files it holds, untracked ones included (#1581), so two stashes
    /// on the same commit can be told apart.
    pub files: Vec<String>,
}

impl StashInfo {
    /// The picker row (#1581): `stash@{N}: <message> · <age> · <files>`,
    /// naming at most three files.
    pub fn label(&self) -> String {
        let mut label = format!("stash@{{{}}}: {}", self.index, self.message);
        if !self.age.is_empty() {
            label.push_str(&format!(" \u{b7} {}", self.age));
        }
        if !self.files.is_empty() {
            let shown: Vec<&str> = self.files.iter().take(3).map(String::as_str).collect();
            label.push_str(&format!(" \u{b7} {}", shown.join(", ")));
            if self.files.len() > 3 {
                label.push_str(&format!(" +{}", self.files.len() - 3));
            }
        }
        label
    }
}

/// List stashes newest-first (`git stash list`), each with its age and
/// files. Two git processes however many stashes there are, since the
/// pickers call this on the UI thread: one `git stash list --name-only`
/// for the tracked files (diffed against the commit the stash was made
/// on) and one `git show` over every stash's untracked-files commit.
pub fn list_stashes(root: &Path) -> Result<Vec<StashInfo>, String> {
    let raw = run_git(
        root,
        &[
            "stash",
            "list",
            "-m",
            "--name-only",
            "--format=%x1e%gd%x1f%cr%x1f%P%x1f%gs",
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    // The third parent, when there is one, holds the untracked files.
    let mut untracked_commits: Vec<(usize, String)> = Vec::new();
    for record in raw.split('\x1e') {
        let mut lines = record.lines();
        let Some(header) = lines.next().filter(|h| !h.is_empty()) else {
            continue;
        };
        let mut parts = header.split('\x1f');
        let reflog = parts.next().unwrap_or("");
        let age = parts.next().unwrap_or("").to_string();
        let parents = parts.next().unwrap_or("");
        let message = parts.next().unwrap_or("").to_string();
        // reflog looks like "stash@{2}" — pull the integer out.
        let index = reflog
            .trim_start_matches("stash@{")
            .trim_end_matches('}')
            .parse::<usize>()
            .unwrap_or(out.len());
        if let Some(untracked) = parents.split_whitespace().nth(2) {
            untracked_commits.push((out.len(), untracked.to_string()));
        }
        let files = lines
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        out.push(StashInfo {
            index,
            message,
            age,
            files,
        });
    }
    if !untracked_commits.is_empty() {
        let mut args = vec!["show", "--root", "--name-only", "--format=%x1e%H"];
        args.extend(untracked_commits.iter().map(|(_, hash)| hash.as_str()));
        if let Ok(raw) = run_git(root, &args) {
            let mut by_hash: std::collections::HashMap<&str, Vec<String>> =
                std::collections::HashMap::new();
            for record in raw.split('\x1e') {
                let mut lines = record.lines();
                let Some(hash) = lines.next().filter(|h| !h.is_empty()) else {
                    continue;
                };
                let names = lines
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                by_hash.insert(hash.trim(), names);
            }
            for (pos, hash) in &untracked_commits {
                if let Some(names) = by_hash.remove(hash.as_str()) {
                    let files = &mut out[*pos].files;
                    files.extend(names);
                    files.sort();
                    files.dedup();
                }
            }
        }
    }
    Ok(out)
}

/// A stash's changes as a patch against the commit it was made on,
/// untracked files included (`git stash show --stat --patch
/// --include-untracked stash@{N}`), for View Stash (#1581).
pub fn show_stash(root: &Path, index: usize) -> Result<String, String> {
    diff_text(
        root,
        &[
            "stash",
            "show",
            "--no-color",
            "--no-ext-diff",
            "--stat",
            "--patch",
            "--include-untracked",
            &format!("stash@{{{index}}}"),
        ],
    )
}

/// Apply a stash without dropping it (`git stash apply stash@{N}`).
pub fn stash_apply(root: &Path, index: usize) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "apply", &format!("stash@{{{index}}}")])
}

/// Apply and drop a specific stash (`git stash pop stash@{N}`).
pub fn stash_pop_at(root: &Path, index: usize) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "pop", &format!("stash@{{{index}}}")])
}

/// Drop a stash without applying it (`git stash drop stash@{N}`).
pub fn stash_drop(root: &Path, index: usize) -> Result<String, String> {
    run_mutation(root, &["stash", "drop", &format!("stash@{{{index}}}")])
}

/// The commit a stash entry records (`git rev-parse stash@{N}`). Unlike its
/// index, it keeps naming the same stash while others are pushed or dropped.
pub fn stash_oid(root: &Path, index: usize) -> Result<String, String> {
    run_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("stash@{{{index}}}"),
        ],
    )
    .map_err(|e| e.to_string())
}

/// Drop the stash whose commit is `oid`, at whatever index it holds now: a
/// stash pushed or dropped since it was picked shifts every index, and the
/// old one would name a different stash. Refuses when it is gone.
pub fn stash_drop_oid(root: &Path, oid: &str) -> Result<String, String> {
    stash_drop(root, stash_index_of(root, oid)?)
}

/// Restore and drop the stash whose commit is `oid`, at whatever index it
/// holds now, as [`stash_drop_oid`] drops one: a stash pushed since, by a
/// checkout hook or another tool, would make a plain `git stash pop` restore
/// that one instead. Refuses when it is gone.
pub fn stash_pop_oid(root: &Path, oid: &str) -> Result<String, String> {
    stash_pop_at(root, stash_index_of(root, oid)?)
}

/// The index the stash whose commit is `oid` holds now, or an error when it
/// is no longer in the stash list.
fn stash_index_of(root: &Path, oid: &str) -> Result<usize, String> {
    let list = run_git(root, &["stash", "list", "--format=%H"]).map_err(|e| e.to_string())?;
    list.lines()
        .position(|hash| hash.trim() == oid)
        .ok_or_else(|| String::from("that stash is no longer in the stash list"))
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

/// Create a tag at HEAD: annotated (`git tag -a <name> -m <message>`)
/// when there is a message, as VS Code's Create Tag makes one (#1582),
/// else lightweight (`git tag <name>`).
pub fn create_tag(root: &Path, name: &str, message: Option<&str>) -> Result<String, String> {
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => run_mutation(root, &["tag", "-a", name, "-m", message]),
        None => run_mutation(root, &["tag", name]),
    }
}

/// Push every local tag to `remote` (`git push <remote> --tags`, #1582).
/// A plain push sends none, so a tag made here stayed local.
pub fn push_tags(root: &Path, remote: &str) -> Result<String, String> {
    run_mutation(root, &["push", remote, "--tags"])
}

/// The tags on `remote` (`git ls-remote --tags --refs`), by name (#1582).
pub fn list_remote_tags(root: &Path, remote: &str) -> Result<Vec<String>, String> {
    let raw = run_mutation(root, &["ls-remote", "--tags", "--refs", remote])?;
    Ok(parse_remote_tags(&raw))
}

/// The tag names in `git ls-remote --tags --refs` output; a peeled
/// `^{}` line, which `--refs` already drops, is the same tag again.
fn parse_remote_tags(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| line.split_once('\t')?.1.strip_prefix("refs/tags/"))
        .filter(|tag| !tag.ends_with("^{}"))
        .map(str::to_string)
        .collect()
}

/// Delete tag `name` from `remote` (`git push <remote> --delete
/// refs/tags/<name>`, #1582); the local tag stays.
pub fn delete_remote_tag(root: &Path, remote: &str, name: &str) -> Result<String, String> {
    run_mutation(
        root,
        &["push", remote, "--delete", &format!("refs/tags/{name}")],
    )
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

/// Which revision a shared link names (#1610): HEAD's commit for Git: Copy
/// Permalink, which keeps pointing at these lines as the branch moves, or
/// the branch for Git: Copy Head Link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRev {
    Commit,
    Branch,
}

/// The web page for `file` at `rev`, scrolled to `lines` (1-based,
/// inclusive), on the current branch's remote, else `origin`, else the only
/// remote (#1610). `Err` says why there is no link rather than handing out
/// one that would not open: no repository or remote, a remote that is not
/// GitHub, GitLab or Bitbucket, a file that was never committed, or a
/// commit or branch that is not on the remote yet.
pub fn share_link(
    file: &Path,
    lines: Option<(usize, usize)>,
    rev: LinkRev,
) -> Result<String, String> {
    let dir = file.parent().ok_or("the file has no folder")?;
    let top = repo_toplevel(dir).ok_or("the file is not in a git repository")?;
    let rel = file
        .canonicalize()
        .ok()
        .zip(top.canonicalize().ok())
        .and_then(|(f, t)| f.strip_prefix(t).ok().map(Path::to_path_buf))
        .ok_or("the file is not in a git repository")?;
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let branch = run_git(&top, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let config = |key: String| run_git(&top, &["config", "--get", &key]).ok();
    let remotes = run_git(&top, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes.lines().collect();
    let remote = branch
        .as_ref()
        .and_then(|b| config(format!("branch.{b}.remote")))
        .filter(|r| remotes.contains(&r.as_str()))
        .or_else(|| remotes.contains(&"origin").then(|| String::from("origin")))
        .or_else(|| (remotes.len() == 1).then(|| remotes[0].to_string()))
        .ok_or("the repository has no remote to link to")?;
    let url = run_git(&top, &["remote", "get-url", &remote])
        .map_err(|_| format!("remote {remote} has no URL"))?;
    let repo = crate::permalink::web_repo(&url)
        .ok_or_else(|| format!("{remote} ({url}) is not a GitHub, GitLab or Bitbucket remote"))?;
    // The file must be in HEAD's tree, which the linked commit or branch
    // contains; being staged is not enough for the remote to show it.
    if run_git(&top, &["cat-file", "-e", &format!("HEAD:{rel}")]).is_err() {
        return Err(format!(
            "{rel} is not committed, so {remote} has no page for it"
        ));
    }
    let rev = match rev {
        LinkRev::Commit => {
            let sha = run_git(&top, &["rev-parse", "HEAD"])
                .map_err(|_| String::from("the repository has no commits"))?;
            let pattern = format!("{remote}/*");
            let pushed = run_git(
                &top,
                &["branch", "-r", "--contains", &sha, "--list", &pattern],
            )
            .is_ok_and(|out| !out.is_empty());
            if !pushed {
                return Err(format!(
                    "commit {} is not on {remote} yet, so the link would not open: push it first",
                    &sha[..sha.len().min(7)]
                ));
            }
            sha
        }
        LinkRev::Branch => {
            let branch = branch.ok_or("HEAD is detached, so there is no branch to link to")?;
            // The branch's name on the remote, which its upstream may spell
            // differently; else the same name.
            let name = config(format!("branch.{branch}.merge"))
                .and_then(|m| m.strip_prefix("refs/heads/").map(str::to_string))
                .unwrap_or_else(|| branch.clone());
            let tracking = format!("refs/remotes/{remote}/{name}");
            if run_git(&top, &["rev-parse", "--verify", "--quiet", &tracking]).is_err() {
                return Err(format!(
                    "branch {name} is not on {remote} yet, so the link would not open: push it first"
                ));
            }
            // A tracking branch behind HEAD would show older lines.
            if run_git(&top, &["merge-base", "--is-ancestor", "HEAD", &tracking]).is_err() {
                return Err(format!(
                    "branch {name} on {remote} is behind your commits, so the link would show older lines: push it first"
                ));
            }
            name
        }
    };
    Ok(crate::permalink::file_url(&repo, &rev, &rel, lines))
}

#[cfg(test)]
mod tests;
