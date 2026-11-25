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
mod remotes;
mod staging;
mod stash;
mod state_watch;
mod status;
mod tags;
mod worker;
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
pub use remotes::LinkRev;
pub use remotes::add_remote;
pub use remotes::clone_into;
pub use remotes::fetch_all;
pub use remotes::list_remotes;
pub use remotes::publish_branch;
pub use remotes::pull_current_branch;
pub use remotes::pull_rebase;
pub use remotes::push_force;
pub use remotes::push_or_publish;
pub use remotes::push_to_remote;
pub use remotes::remove_remote;
pub use remotes::share_link;
pub use remotes::unpublished_branch;
use remotes::*;
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
pub use stash::StashKind;
pub use stash::list_stashes;
pub use stash::show_stash;
pub use stash::stash_apply;
pub use stash::stash_drop_oid;
pub use stash::stash_oid;
pub use stash::stash_pop;
pub use stash::stash_pop_at;
pub use stash::stash_pop_oid;
pub use stash::stash_push;
pub use stash::stash_push_staged;
pub use stash::stash_push_untracked;
#[cfg(test)]
use stash::*;
pub use state_watch::GitStateWatch;
#[cfg(test)]
use state_watch::*;
pub use status::GitStatus;
pub use status::child_repos;
pub use status::is_ignored;
pub use status::query;
use status::*;
pub use tags::create_tag;
pub use tags::delete_remote_tag;
pub use tags::delete_tag;
pub use tags::list_remote_tags;
pub use tags::list_tags;
pub use tags::push_tags;
pub use tags::tag_remote;
#[cfg(test)]
use tags::*;
pub use worker::GitRequest;
pub use worker::GitResponse;
pub use worker::git_worker_loop;
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
    let mut cmd = crate::proc::command("git");
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
    let mut cmd = crate::proc::command("git");
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
    let mut cmd = crate::proc::command("git");
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
    let output = crate::proc::command("git")
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

#[cfg(test)]
mod recorded_calls;
#[cfg(test)]
mod tests;
