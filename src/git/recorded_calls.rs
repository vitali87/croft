//! Every git call the git module makes, recorded (#2062, card 12).
//!
//! One scenario drives each of the module's entry points against a fixture
//! repository, with `git` replaced by a stand-in that writes down what croft
//! handed it and then runs the real git. The record is saved in
//! `src/app/tests/golden/git-calls.txt`. A change to how croft starts git
//! must leave it byte-identical, unless the change is meant to move a call,
//! which `CROFT_BLESS=1` then writes down.
//!
//! The real git runs shut off from the developer's own setup (no global or
//! system config) and with one fixed date and identity, so every commit, and
//! every hash an argument carries, is the same on every machine.

#![cfg(target_os = "linux")]

use super::*;
use crate::test_git::test_git;
use std::path::{Path, PathBuf};

const SAVED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/app/tests/golden/git-calls.txt"
);

/// The date of every commit, in the fixture and made by croft.
const DATE: &str = "2099-01-01T00:00:00Z";

/// git for building the fixture, outside the recording.
fn fixture(dir: &Path, args: &[&str]) -> String {
    let out = test_git(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// The record so far, one titled section per step.
struct Script {
    recording: crate::proc::record::Recording,
    names: Vec<(PathBuf, &'static str)>,
    out: String,
}

impl Script {
    /// Write down, under `title`, every git run since the last step.
    fn step(&mut self, title: &str) {
        let names: Vec<(&Path, &str)> = self
            .names
            .iter()
            .map(|(path, name)| (path.as_path(), *name))
            .collect();
        let runs = self.recording.take(&names);
        self.out.push_str(&format!("## {title}\n"));
        for run in runs {
            self.out.push_str(&run);
            self.out.push('\n');
        }
    }
}

/// The variable that names where [`child_records_every_git_call`] writes
/// its record; the test does nothing without it.
const RECORD_TO: &str = "CROFT_RECORD_GIT_CALLS_TO";

/// The scenario runs in a fresh copy of this test binary with an empty
/// environment but `PATH` and a scratch `HOME`. Run here, a variable the
/// machine already sets, as a CI runner or a sandbox does
/// (`GIT_TERMINAL_PROMPT`, `GIT_EDITOR`, `LANGUAGE`), hides croft setting the
/// same value, so the record would differ from machine to machine.
#[test]
fn every_git_call_is_made_as_recorded() {
    let home = tempfile::tempdir().unwrap();
    let record = home.path().join("git-calls.txt");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "git::recorded_calls::child_records_every_git_call",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env(RECORD_TO, &record)
        .output()
        .unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the recording failed:\n{log}");
    assert!(
        log.contains("1 passed"),
        "the recording did not run:\n{log}"
    );
    let now = std::fs::read_to_string(&record).unwrap();
    crate::proc::record::check_saved(Path::new(SAVED), &now);
}

/// The scenario, run by [`every_git_call_is_made_as_recorded`].
#[test]
fn child_records_every_git_call() {
    let Some(record_to) = std::env::var_os(RECORD_TO) else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path().canonicalize().unwrap();
    let bin = tmp.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let repo = tmp.join("repo");
    let origin = tmp.join("origin.git");
    std::fs::create_dir(&repo).unwrap();

    // The fixture: main with two commits and a tag on the first, a
    // `feature` branch off the first, and a bare `origin` both are pushed to.
    fixture(&tmp, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    fixture(&repo, &["init", "-q", "-b", "main"]);
    write(&repo.join("a.txt"), "one\n");
    write(&repo.join("b.txt"), "two\n");
    write(&repo.join("dir/c.txt"), "three\n");
    fixture(&repo, &["add", "-A"]);
    fixture(&repo, &["commit", "-q", "-m", "first"]);
    let first = fixture(&repo, &["rev-parse", "HEAD"]);
    fixture(&repo, &["tag", "v1"]);
    write(&repo.join("a.txt"), "one\nmore\n");
    fixture(&repo, &["commit", "-q", "-am", "second"]);
    let second = fixture(&repo, &["rev-parse", "HEAD"]);
    fixture(&repo, &["checkout", "-q", "-b", "feature", &first]);
    write(&repo.join("b.txt"), "two\nfeature\n");
    fixture(&repo, &["commit", "-q", "-am", "feature work"]);
    let feature = fixture(&repo, &["rev-parse", "HEAD"]);
    fixture(&repo, &["checkout", "-q", "main"]);
    fixture(
        &repo,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    fixture(&repo, &["push", "-q", "-u", "origin", "main", "feature"]);

    let real = crate::proc::record::real_program("git");
    let then = format!(
        "exec env -u GIT_CONFIG_PARAMETERS -u GIT_CONFIG_COUNT \
         GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME=/dev/null \
         GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@b GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@b \
         GIT_AUTHOR_DATE={DATE} GIT_COMMITTER_DATE={DATE} '{}' \"$@\"",
        real.display()
    );
    let mut s = Script {
        recording: crate::proc::record::record(&bin, "git", &then),
        names: vec![
            (repo.clone(), "<repo>"),
            (origin.clone(), "<origin>"),
            (tmp.clone(), "<tmp>"),
        ],
        out: String::from(
            "# Every git call the git module makes: src/git/recorded_calls.rs. \
             Rewrite with CROFT_BLESS=1 after a change meant to move one.\n",
        ),
    };
    let rel = |paths: &[&str]| -> Vec<String> { paths.iter().map(|p| p.to_string()).collect() };

    // Status.
    let _ = repo_toplevel(&repo.join("dir"));
    s.step("repo_toplevel");
    let _ = query(&repo);
    s.step("status::query");
    let _ = query_changes(&repo);
    s.step("query_changes");
    let _ = is_ignored(&repo, &repo.join("a.txt"));
    s.step("is_ignored");
    let _ = child_repos(&tmp);
    s.step("child_repos");
    let _ = worktree_fingerprint(&repo);
    s.step("worktree_fingerprint");
    let _ = current_branch(&repo);
    s.step("current_branch");
    let _ = head_is_on_upstream(&repo);
    s.step("head_is_on_upstream");
    let _ = unpublished_branch(&repo);
    s.step("unpublished_branch");
    let _ = repo_operation(&repo);
    s.step("repo_operation");

    // Diffs and file contents, with a staged and an unstaged change.
    write(&repo.join("b.txt"), "two\nstaged\n");
    fixture(&repo, &["add", "b.txt"]);
    write(&repo.join("a.txt"), "one\nmore\nunstaged\n");
    let _ = diff_staged(&repo);
    s.step("diff_staged");
    let _ = merge_base(&repo, "feature");
    s.step("merge_base");
    let _ = diff_against_branch(&repo, "feature");
    s.step("diff_against_branch");
    let _ = diff_previous_commit(&repo);
    s.step("diff_previous_commit");
    let _ = is_submodule(&repo, "a.txt");
    s.step("is_submodule");
    let _ = submodule_diff(&repo, "a.txt");
    s.step("submodule_diff");
    let _ = contents::path_filter(&repo, "a.txt");
    s.step("path_filter");
    let _ = read_file_at_head(&repo, "a.txt");
    s.step("read_file_at_head");
    let _ = read_bytes_at_head(&repo, "a.txt");
    s.step("read_bytes_at_head");
    let _ = read_file_at_rev(&repo, "v1", "a.txt");
    s.step("read_file_at_rev");
    let _ = read_file_at_stage(&repo, "a.txt", 2);
    s.step("read_file_at_stage");
    let _ = remerge_diff3(&repo, "a.txt");
    s.step("remerge_diff3");
    let _ = query(&repo);
    s.step("status::query with changes");
    let _ = query_changes(&repo);
    s.step("query_changes with changes");

    // History and blame.
    let _ = file_history(&repo, "a.txt", 10);
    s.step("file_history");
    let _ = show_commit_file_diff(&repo, &second, "a.txt");
    s.step("show_commit_file_diff");
    let _ = follow_names(&repo, "a.txt", 10);
    s.step("follow_names");
    let _ = show_commit(&repo, &second);
    s.step("show_commit");
    let _ = commit_graph(&repo, 50);
    s.step("commit_graph");
    let _ = branch_history(&repo, 50);
    s.step("branch_history");
    let _ = branch_history_range(&repo, "main..feature", 50);
    s.step("branch_history_range");
    let _ = tree_paths(&repo, "HEAD");
    s.step("tree_paths");
    let _ = commit_message(&repo, &second);
    s.step("commit_message");
    let _ = commit_card(&repo, &second);
    s.step("commit_card");
    let _ = blame(&repo, "a.txt");
    s.step("blame");

    // Staging and patches.
    write(&repo.join("d.txt"), "new\n");
    let _ = stage_path(&repo, "a.txt");
    s.step("stage_path");
    let _ = unstage_path(&repo, "a.txt");
    s.step("unstage_path");
    let _ = stage_paths(&repo, &rel(&["a.txt", "d.txt"]));
    s.step("stage_paths");
    let _ = unstage_paths(&repo, &rel(&["a.txt", "d.txt"]));
    s.step("unstage_paths");
    let _ = discard_path(&repo, "d.txt", true, false);
    s.step("discard_path untracked");
    let _ = discard_path(&repo, "a.txt", false, false);
    s.step("discard_path");
    let _ = discard_path(&repo, "b.txt", false, true);
    s.step("discard_path staged");
    let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n one\n more\n+patched\n";
    let _ = apply_patch(&repo, patch, false, false);
    s.step("apply_patch");
    let _ = apply_patch(&repo, patch, true, false);
    s.step("apply_patch cached");
    let _ = apply_patch(&repo, patch, true, true);
    s.step("apply_patch cached reverse");
    let _ = stage_all(&repo);
    s.step("stage_all");
    let _ = unstage_all(&repo);
    s.step("unstage_all");

    // Commits.
    let _ = commit(&repo, "all of it");
    s.step("commit");
    write(&repo.join("a.txt"), "one\nmore\npatched\nstaged\n");
    fixture(&repo, &["add", "a.txt"]);
    let _ = commit_staged(&repo, "staged only");
    s.step("commit_staged");
    let _ = commit_amend(&repo, "staged, amended");
    s.step("commit_amend");
    let _ = commit_amend_no_edit(&repo);
    s.step("commit_amend_no_edit");
    let _ = undo_last_commit(&repo);
    s.step("undo_last_commit");
    let _ = unmerged_paths(&repo);
    s.step("unmerged_paths");
    let _ = refuse_unmerged(&repo);
    s.step("refuse_unmerged");
    let _ = discard_all_tracked(&repo);
    s.step("discard_all_tracked");
    let _ = cherry_pick(&repo, &feature);
    s.step("cherry_pick");
    let _ = revert_commit(&repo, &second);
    s.step("revert_commit");
    let _ = create_tag_at(&repo, "t2", &first);
    s.step("create_tag_at");
    let _ = checkout_detached(&repo, &first);
    s.step("checkout_detached");
    let _ = checkout_branch(&repo, "main");
    s.step("checkout_branch");
    let _ = reset_to(&repo, &second, ResetMode::Mixed);
    s.step("reset_to mixed");
    let _ = reset_to(&repo, &second, ResetMode::Hard);
    s.step("reset_to hard");
    let _ = remove_path(&repo, "dir/c.txt");
    s.step("remove_path");
    fixture(&repo, &["reset", "-q", "--hard", "HEAD"]);

    // Branches.
    let _ = default_branch(&repo);
    s.step("default_branch");
    let _ = list_branches(&repo);
    s.step("list_branches");
    let _ = create_branch(&repo, "topic");
    s.step("create_branch");
    let _ = rename_branch(&repo, "topic-renamed");
    s.step("rename_branch");
    let _ = checkout_branch(&repo, "main");
    s.step("checkout_branch back");
    let _ = create_branch_from(&repo, "from-v1", "v1");
    s.step("create_branch_from");
    let _ = checkout_branch(&repo, "main");
    s.step("checkout_branch main");
    let _ = track_remote_branch(&repo, "origin/feature", "feature-local");
    s.step("track_remote_branch");
    let _ = checkout_branch(&repo, "main");
    s.step("checkout_branch main again");
    let _ = delete_branch(&repo, "topic-renamed");
    s.step("delete_branch");
    let _ = merge_branch(&repo, "feature");
    s.step("merge_branch");
    let _ = rebase_branch(&repo, "feature");
    s.step("rebase_branch");
    fixture(&repo, &["reset", "-q", "--hard", &second]);

    // Tags.
    let _ = tag_remote(&repo);
    s.step("tag_remote");
    let _ = list_tags(&repo);
    s.step("list_tags");
    let _ = create_tag(&repo, "v2", Some("second release"));
    s.step("create_tag");
    let _ = push_tags(&repo, "origin");
    s.step("push_tags");
    let _ = list_remote_tags(&repo, "origin");
    s.step("list_remote_tags");
    let _ = delete_remote_tag(&repo, "origin", "v2");
    s.step("delete_remote_tag");
    let _ = delete_tag(&repo, "v2");
    s.step("delete_tag");

    // Stashes.
    write(&repo.join("a.txt"), "stashed\n");
    let _ = stash_push(&repo, Some("first stash"));
    s.step("stash_push");
    let _ = list_stashes(&repo);
    s.step("list_stashes");
    let _ = show_stash(&repo, 0);
    s.step("show_stash");
    let _ = stash_apply(&repo, 0);
    s.step("stash_apply");
    let oid = stash_oid(&repo, 0).unwrap_or_default();
    s.step("stash_oid");
    let _ = stash_drop_oid(&repo, &oid);
    s.step("stash_drop_oid");
    write(&repo.join("e.txt"), "untracked\n");
    let _ = stash_push_untracked(&repo, None);
    s.step("stash_push_untracked");
    let _ = stash_pop(&repo);
    s.step("stash_pop");
    fixture(&repo, &["add", "a.txt"]);
    let _ = stash_push_staged(&repo, Some("staged stash"));
    s.step("stash_push_staged");
    let _ = stash_pop_at(&repo, 0);
    s.step("stash_pop_at");
    let _ = stash_push(&repo, None);
    s.step("stash_push again");
    let oid = stash_oid(&repo, 0).unwrap_or_default();
    s.step("stash_oid again");
    let _ = stash_pop_oid(&repo, &oid);
    s.step("stash_pop_oid");
    fixture(&repo, &["reset", "-q", "--hard", "HEAD"]);
    fixture(&repo, &["clean", "-q", "-f"]);

    // Remotes.
    let _ = list_remotes(&repo);
    s.step("list_remotes");
    let _ = add_remote(&repo, "other", &origin.display().to_string());
    s.step("add_remote");
    let _ = remove_remote(&repo, "other");
    s.step("remove_remote");
    let _ = fetch_all(&repo);
    s.step("fetch_all");
    write(&repo.join("a.txt"), "one\nmore\npushed\n");
    fixture(&repo, &["commit", "-q", "-am", "to push"]);
    let _ = push_current_branch(&repo);
    s.step("push_current_branch");
    let _ = pull_current_branch(&repo);
    s.step("pull_current_branch");
    let _ = pull_rebase(&repo);
    s.step("pull_rebase");
    let _ = push_force(&repo);
    s.step("push_force");
    let _ = push_to_remote(&repo, "origin");
    s.step("push_to_remote");
    fixture(&repo, &["checkout", "-q", "-b", "unpushed"]);
    let _ = unpublished_branch(&repo);
    s.step("unpublished_branch on a new branch");
    let _ = push_or_publish(&repo);
    s.step("push_or_publish");
    fixture(&repo, &["checkout", "-q", "-b", "to-publish"]);
    let _ = publish_branch(&repo, "to-publish");
    s.step("publish_branch");
    fixture(&repo, &["checkout", "-q", "main"]);
    let _ = clone_into(&tmp, &origin.display().to_string());
    s.step("clone_into");
    let _ = share_link(&repo.join("a.txt"), Some((1, 2)), LinkRev::Commit);
    s.step("share_link commit");
    let _ = share_link(&repo.join("a.txt"), None, LinkRev::Branch);
    s.step("share_link branch");

    // Worktrees.
    let mut lane = worktrees::WorktreeLane::plan(&repo, "lane one").unwrap();
    let _ = add_worktree_lane(&repo, &mut lane);
    s.step("add_worktree_lane");
    let _ = lane_removal_block(&lane.path);
    s.step("lane_removal_block");
    let _ = worktrees::worktree_head(&lane.path);
    s.step("worktree_head");
    let _ = pr_worktree_keep_reason(&lane.path, "main");
    s.step("pr_worktree_keep_reason");
    let _ = remove_worktree_lane(&lane.path);
    s.step("remove_worktree_lane");
    let _ = pr_worktree_path(&repo, 7);
    s.step("pr_worktree_path");
    let _ = remote_for_slug(&repo, "owner/repo");
    s.step("remote_for_slug");
    let _ = checkout_pr_worktree(&repo, "origin", 7, &tmp.join("pr-7"), None);
    s.step("checkout_pr_worktree");

    // Operations in progress: a merge that conflicts, aborted; a
    // cherry-pick that conflicts, resolved and continued; one skipped; then
    // a bisect, reset.
    fixture(&repo, &["checkout", "-q", "-b", "clash", &first]);
    write(&repo.join("a.txt"), "clash\n");
    fixture(&repo, &["commit", "-q", "-am", "clash"]);
    let clash = fixture(&repo, &["rev-parse", "HEAD"]);
    fixture(&repo, &["checkout", "-q", "main"]);
    let _ = merge_branch(&repo, "clash");
    s.step("merge_branch that conflicts");
    let _ = repo_operation(&repo);
    s.step("repo_operation during a merge");
    let _ = unmerged_paths(&repo);
    s.step("unmerged_paths during a merge");
    let _ = read_file_at_stage(&repo, "a.txt", 3);
    s.step("read_file_at_stage during a merge");
    let _ = remerge_diff3(&repo, "a.txt");
    s.step("remerge_diff3 during a merge");
    let _ = refuse_unmerged(&repo);
    s.step("refuse_unmerged during a merge");
    let _ = stage_path(&repo, "a.txt");
    s.step("stage_path with conflict markers");
    let _ = continue_operation(&repo, RepoOp::Merge);
    s.step("continue_operation of a merge");
    let _ = abort_operation(&repo, RepoOp::Merge);
    s.step("abort_operation of a merge");

    let _ = cherry_pick(&repo, &clash);
    s.step("cherry_pick that conflicts");
    let _ = repo_operation(&repo);
    s.step("repo_operation during a cherry-pick");
    write(&repo.join("a.txt"), "resolved\n");
    fixture(&repo, &["add", "a.txt"]);
    let _ = continue_operation(&repo, RepoOp::CherryPick);
    s.step("continue_operation of a cherry-pick");
    fixture(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    let _ = cherry_pick(&repo, &clash);
    s.step("cherry_pick that conflicts again");
    let _ = skip_operation(&repo, RepoOp::CherryPick);
    s.step("skip_operation of a cherry-pick");

    fixture(&repo, &["bisect", "start", "HEAD", &first]);
    let _ = bisect_mark(&repo, true);
    s.step("bisect_mark good");
    let _ = bisect_mark(&repo, false);
    s.step("bisect_mark bad");
    let _ = repo_operation(&repo);
    s.step("repo_operation during a bisect");
    let _ = abort_operation(&repo, RepoOp::Bisect);
    s.step("abort_operation of a bisect");

    std::fs::write(record_to, &s.out).unwrap();
}
