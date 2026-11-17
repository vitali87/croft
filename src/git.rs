use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

mod blame;
mod changes;
mod contents;
mod diffs;
mod history;
mod messages;
mod operations;
mod patches;
mod status;
mod worktrees;
pub use blame::Blame;
pub use blame::BlameCommit;
pub use blame::blame;
#[cfg(test)]
use blame::*;
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
pub use diffs::diff_against_branch;
pub use diffs::diff_previous_commit;
pub use diffs::diff_staged;
pub use diffs::is_submodule;
pub use diffs::merge_base;
pub use diffs::submodule_diff;
use diffs::*;
pub use history::CommitTree;
pub use history::FileHistoryEntry;
pub use history::GraphCommit;
pub use history::branch_history;
pub use history::branch_history_range;
pub use history::commit_card;
pub use history::commit_graph;
pub use history::commit_message;
pub use history::file_history;
pub use history::follow_names;
pub use history::humanize_age;
pub use history::show_commit;
pub use history::show_commit_file_diff;
pub use history::tree_paths;
use history::*;
pub use messages::error_line;
pub use messages::failure_line;
pub use messages::headline;
pub use messages::local_changes_block_switch;
pub use messages::result_line;
pub use messages::summary_line;
#[cfg(test)]
use messages::*;
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
pub use worktrees::PrWorktree;
pub use worktrees::WorktreeLane;
pub use worktrees::add_worktree_lane;
pub use worktrees::checkout_pr_worktree;
pub use worktrees::lane_removal_block;
pub use worktrees::pr_worktree_keep_reason;
pub use worktrees::pr_worktree_path;
pub use worktrees::remote_for_slug;
pub use worktrees::remove_worktree_lane;
#[cfg(test)]
use worktrees::*;

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

/// The branch HEAD is on, or `None` when detached.
///
/// `--quiet` so a detached HEAD is an empty result rather than an error the
/// caller has to distinguish from a failure.
pub fn current_branch(root: &Path) -> Option<String> {
    let out = run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| name.to_string())
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
