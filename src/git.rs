use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

mod blame;
mod branches;
mod changes;
mod commits;
mod contents;
mod diffs;
mod history;
mod messages;
mod operations;
mod patches;
mod staging;
mod status;
mod worktrees;
pub use blame::Blame;
pub use blame::BlameCommit;
pub use blame::blame;
#[cfg(test)]
use blame::*;
pub use branches::BranchInfo;
pub use branches::checkout_branch;
pub use branches::create_branch;
pub use branches::create_branch_from;
pub use branches::current_branch;
pub use branches::default_branch;
pub use branches::delete_branch;
pub use branches::list_branches;
pub use branches::merge_branch;
pub use branches::rebase_branch;
pub use branches::rename_branch;
pub use branches::track_remote_branch;
pub use changes::ChangeEntry;
pub use changes::ChangeKind;
pub use changes::ChangeSection;
pub use changes::GitDecoration;
pub use changes::query_changes;
pub use changes::unquote_porcelain_path;
use changes::*;
pub use commits::ResetMode;
pub use commits::checkout_detached;
pub use commits::cherry_pick;
pub use commits::commit;
pub use commits::commit_amend;
pub use commits::commit_amend_no_edit;
pub use commits::commit_staged;
pub use commits::create_tag_at;
pub use commits::head_is_on_upstream;
pub use commits::refuse_unmerged;
pub use commits::reset_to;
pub use commits::revert_commit;
pub use commits::undo_last_commit;
#[cfg(test)]
pub use commits::unmerged_paths;
pub use commits::worktree_fingerprint;
use commits::*;
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
pub use staging::discard_all_tracked;
pub use staging::discard_path;
pub use staging::remove_path;
pub use staging::stage_all;
pub use staging::stage_path;
pub use staging::stage_paths;
pub use staging::unstage_all;
pub use staging::unstage_path;
#[cfg(test)]
use staging::*;
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

/// Pull the current branch from its upstream (`git pull`). Sibling of
/// `push_current_branch`; surfaced by the commit dropdown's "Pull" item
/// and used by `sync` below. Returns git's verbatim summary ("Already
/// up to date." / the fast-forward range) so the panel can echo it.
pub fn pull_current_branch(root: &Path) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["pull"])
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
