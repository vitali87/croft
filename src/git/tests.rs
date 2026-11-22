//! git.rs's own tests, moved out of git.rs (#2062).

/// #858: the one line reported for git's output. A new branch's ref
/// update loses git's `*` flag but keeps its `[new branch]` tag; a
/// message with no ref update is its first line past the noise; git's
/// text of only noise is empty, and `error_line` then falls back to the
/// first line rather than saying nothing.
#[test]
fn summary_line_picks_the_ref_update_or_the_first_meaningful_line() {
    assert_eq!(
        summary_line("To /tmp/r.git\n * [new branch]      main -> main"),
        "[new branch] main -> main"
    );
    assert_eq!(
        summary_line("Everything up-to-date"),
        "Everything up-to-date"
    );
    assert_eq!(
        summary_line("On branch main\nYour branch is up to date.\n\nnothing to commit"),
        "nothing to commit"
    );
    assert_eq!(summary_line("remote: a\nremote: b\nhint: c"), "");
    assert_eq!(error_line("hint: only a hint"), "hint: only a hint");
}

/// Git 2.43's text for a `git merge feature` that stopped on a conflict
/// (all of it on stdout).
const MERGE_CONFLICT_858: &str = "Auto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nAutomatic merge failed; fix conflicts and then commit the result.";
/// The same merge with three conflicted files.
const MERGE_CONFLICTS_858: &str = "Auto-merging a.py\nCONFLICT (content): Merge conflict in a.py\nAuto-merging b.py\nCONFLICT (content): Merge conflict in b.py\nAuto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nAutomatic merge failed; fix conflicts and then commit the result.";
/// `git rebase feature` stopped on a conflict: stdout (the conflict),
/// then stderr, whose first line git redrew over its `Rebasing (1/1)`
/// progress with a carriage return.
const REBASE_CONFLICT_858: &str = "Auto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nRebasing (1/1)\rerror: could not apply ffb4d23... main: raise free shipping\nhint: Resolve all conflicts manually, mark them as resolved with\nhint: \"git add/rm <conflicted_files>\", then run \"git rebase --continue\".\nhint: You can instead skip this commit: run \"git rebase --skip\".\nhint: To abort and get back to the state before \"git rebase\", run \"git rebase --abort\".\nCould not apply ffb4d23... main: raise free shipping";
/// A `git pull` that fetched, then stopped merging on a conflict:
/// stdout, then the fetch's ref updates from stderr.
const PULL_CONFLICT_858: &str = "Auto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nAutomatic merge failed; fix conflicts and then commit the result.\nFrom github.com:o/r\n   5a6cd5c..9f1e2d3  main       -> origin/main";
/// `git stash pop` onto a conflicting commit.
const STASH_POP_CONFLICT_858: &str = "Auto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nOn branch main\nUnmerged paths:\n  (use \"git restore --staged <file>...\" to unstage)\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   pricing.py\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\nThe stash entry is kept in case you need it again.";

/// #858: a merge, rebase, pull or stash pop that stopped on conflicts
/// is reported by its conflicts, not by git's first line, which is
/// `Auto-merging pricing.py` and reads like a step that went fine.
#[test]
fn error_line_names_the_conflicts_git_stopped_on() {
    assert_eq!(error_line(MERGE_CONFLICT_858), "1 conflict (pricing.py)");
    assert_eq!(
        error_line(MERGE_CONFLICTS_858),
        "3 conflicts (a.py, b.py, pricing.py)"
    );
    assert_eq!(error_line(REBASE_CONFLICT_858), "1 conflict (pricing.py)");
    assert_eq!(
        error_line(PULL_CONFLICT_858),
        "1 conflict (pricing.py)",
        "the conflict, not the fetch's ref update"
    );
    assert_eq!(
        error_line(STASH_POP_CONFLICT_858),
        "1 conflict (pricing.py)"
    );
}

/// #858: with no conflict to name, git's `error:` or `fatal:` line wins
/// over a progress line or a fetched ref update printed before it, and
/// a line git redrew with a carriage return reads as its last redraw.
#[test]
fn error_line_prefers_git_s_error_and_fatal_lines() {
    let rebase_stderr = "Rebasing (1/1)\rerror: could not apply ffb4d23... main: raise free shipping\nhint: Resolve all conflicts manually, mark them as resolved with\nCould not apply ffb4d23... main: raise free shipping";
    assert_eq!(
        error_line(rebase_stderr),
        "error: could not apply ffb4d23... main: raise free shipping"
    );
    let pull = "From github.com:o/r\n * branch            main       -> FETCH_HEAD\nhint: Diverging branches can't be fast-forwarded.\nfatal: Not possible to fast-forward, aborting.";
    assert_eq!(
        error_line(pull),
        "fatal: Not possible to fast-forward, aborting."
    );
}

/// #858: git's result line, not the `Auto-merging` note printed before
/// it or the `Rebasing (1/1)` progress it redrew with a carriage return
/// and an erase-line sequence.
#[test]
fn headline_skips_auto_merging_and_reads_a_redrawn_line_as_shown() {
    assert_eq!(
        headline(
            "Auto-merging pricing.py\nMerge made by the 'ort' strategy.\n pricing.py | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)"
        ),
        "Merge made by the 'ort' strategy."
    );
    assert_eq!(
        headline("Rebasing (1/1)\r\u{1b}[KSuccessfully rebased and updated refs/heads/rb."),
        "Successfully rebased and updated refs/heads/rb."
    );
}

/// #858 negative: a text of only `Auto-merging` notes still reports
/// something, never an empty line.
#[test]
fn error_line_of_only_auto_merging_notes_is_never_empty() {
    assert_eq!(
        error_line("Auto-merging pricing.py"),
        "Auto-merging pricing.py"
    );
    let two = error_line("Auto-merging a.py\nAuto-merging b.py");
    assert!(!two.is_empty(), "{two:?}");
    assert!(!headline("Auto-merging pricing.py").is_empty());
}

/// #858 negative: the lines the push and commit reports already picked
/// are unchanged: a rejected ref update over git's `error:` line after
/// it, the first `fatal:` line, and "nothing to commit" past the
/// `On branch` preamble.
#[test]
fn error_line_keeps_the_push_and_commit_lines_it_already_picked() {
    let rejected = "To github.com:o/r.git\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs to 'github.com:o/r.git'\nhint: Updates were rejected because the remote contains work that you do";
    assert_eq!(
        error_line(rejected),
        "[rejected] main -> main (fetch first)"
    );
    let fatal = "fatal: 'origin' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights";
    assert_eq!(
        error_line(fatal),
        "fatal: 'origin' does not appear to be a git repository"
    );
    assert_eq!(
        error_line(
            "On branch main\nYour branch is up to date with 'origin/main'.\n\nnothing to commit, working tree clean"
        ),
        "nothing to commit, working tree clean"
    );
    assert_eq!(
        headline("[main 5a6cd5c] fix: one line\n 1 file changed, 1 insertion(+)"),
        "[main 5a6cd5c] fix: one line"
    );
}

/// git 2.43's refusal to switch branches over a local edit.
const SWITCH_REFUSED_925: &str = "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt\nPlease commit your changes or stash them before you switch branches.\nAborting";

/// #925: a refusal that lists files under its `error:` line names them
/// (three at most) instead of stopping at the colon, and is the one a
/// stash gets past; a switch blocked by untracked files is not.
#[test]
fn error_line_names_the_files_git_listed_under_it() {
    assert_eq!(
        error_line(SWITCH_REFUSED_925),
        "error: Your local changes to the following files would be overwritten by checkout: a.txt"
    );
    let many = SWITCH_REFUSED_925.replace("\ta.txt", "\ta.txt\n\tb.txt\n\tc.txt\n\td.txt");
    assert_eq!(
        error_line(&many),
        "error: Your local changes to the following files would be overwritten by checkout: a.txt, b.txt, c.txt, \u{2026}"
    );
    assert!(local_changes_block_switch(SWITCH_REFUSED_925));
    let untracked = "error: The following untracked working tree files would be overwritten by checkout:\n\tb.txt\nPlease move or remove them before you switch branches.\nAborting";
    assert_eq!(
        error_line(untracked),
        "error: The following untracked working tree files would be overwritten by checkout: b.txt"
    );
    assert!(!local_changes_block_switch(untracked));
    // Both at once, as git 2.43 words it: a stash would not clear it.
    let both = SWITCH_REFUSED_925.replace("\nAborting", "\n") + untracked;
    assert!(!local_changes_block_switch(&both));
}

/// #858: the conflicts are counted per file and named, three at most;
/// a lone conflict git words without a file is git's own line; a text
/// with no `CONFLICT` line has no conflict summary.
#[test]
fn conflict_summary_counts_and_names_the_conflicted_files() {
    assert_eq!(
        conflict_summary(MERGE_CONFLICT_858).as_deref(),
        Some("1 conflict (pricing.py)")
    );
    let five = (1..=5)
        .map(|i| format!("Auto-merging f{i}.py\nCONFLICT (content): Merge conflict in f{i}.py"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        conflict_summary(&five).as_deref(),
        Some("5 conflicts (f1.py, f2.py, f3.py, \u{2026})")
    );
    let twice =
        "CONFLICT (add/add): Merge conflict in a.py\nCONFLICT (content): Merge conflict in a.py";
    assert_eq!(
        conflict_summary(twice).as_deref(),
        Some("1 conflict (a.py)")
    );
    let modify_delete = "CONFLICT (modify/delete): report.py deleted in feature and modified in HEAD.  Version HEAD of report.py left in tree.\nAutomatic merge failed; fix conflicts and then commit the result.";
    assert_eq!(
        conflict_summary(modify_delete).as_deref(),
        Some(
            "CONFLICT (modify/delete): report.py deleted in feature and modified in HEAD. Version HEAD of report.py left in tree."
        )
    );
    let mixed = format!("{modify_delete}\n{MERGE_CONFLICT_858}");
    assert_eq!(
        conflict_summary(&mixed).as_deref(),
        Some("2 conflicts (pricing.py, \u{2026})")
    );
    assert_eq!(conflict_summary("Auto-merging pricing.py"), None);
    assert_eq!(
        conflict_summary("error: cannot rebase: You have unstaged changes."),
        None
    );
}

/// #858: a failure names the operation ("Merge failed", never the
/// "Merged failed" its success prefix made of it); one that stopped on
/// conflicts names them after the operation.
#[test]
fn failure_line_names_the_operation_and_why() {
    assert_eq!(
        failure_line("Merge", MERGE_CONFLICT_858),
        "Merge: 1 conflict (pricing.py)"
    );
    assert_eq!(
        failure_line("Rebase", REBASE_CONFLICT_858),
        "Rebase: 1 conflict (pricing.py)"
    );
    assert_eq!(
        failure_line(
            "Rebase",
            "error: cannot rebase: You have unstaged changes.\nerror: Please commit or stash them."
        ),
        "Rebase failed: error: cannot rebase: You have unstaged changes."
    );
    assert_eq!(
        failure_line("Merge", "merge: nosuch - not something we can merge"),
        "Merge failed: merge: nosuch - not something we can merge"
    );
    assert_eq!(
        failure_line("Merge", "Auto-merging pricing.py"),
        "Merge failed: Auto-merging pricing.py",
        "only noise: still says something, and still that the merge failed"
    );
}
/// #858: the line a successful pull, fetch, push, stash or switch
/// reports, from git's own texts (2.43): its result, past the fetch and
/// push chatter, the range, the diffstat and `git status` dumps.
#[test]
fn result_line_names_what_git_did() {
    let fast_forward =
        "Updating 5d25c91..f79a881\nFast-forward\n f.txt | 1 +\n 1 file changed, 1 insertion(+)";
    assert_eq!(result_line(fast_forward), "Fast-forward");
    let rebased = "From /tmp/r\n   5d25c91..f79a881  main       -> origin/main\nRebasing (1/2)\rRebasing (2/2)\r\r\u{1b}[KSuccessfully rebased and updated refs/heads/main.";
    assert_eq!(
        result_line(rebased),
        "Successfully rebased and updated refs/heads/main."
    );
    assert_eq!(
        result_line("Auto-merging f.txt\nMerge made by the 'ort' strategy.\n f.txt | 1 +"),
        "Merge made by the 'ort' strategy."
    );
    let popped = "On branch main\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n\tmodified:   g.txt\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\nDropped refs/stash@{0} (4f2e1d0)";
    assert_eq!(result_line(popped), "Dropped refs/stash@{0} (4f2e1d0)");
    assert_eq!(
        result_line("[main 5a6cd5c] fix: a -> b  twice\n 1 file changed"),
        "[main 5a6cd5c] fix: a -> b  twice",
        "a commit's subject is kept as written"
    );
    let fetched = "From /tmp/r\n   a3fecf9..117b94a  main       -> origin/main";
    assert_eq!(result_line(fetched), "a3fecf9..117b94a main -> origin/main");
    let negotiated = "fatal: expected 'acknowledgments', received 'packfile'\nwarning: push negotiation failed; proceeding anyway with push\nTo /tmp/r.git\n + 915bd5e...74d526d main -> main (forced update)";
    assert_eq!(
        result_line(negotiated),
        "915bd5e...74d526d main -> main (forced update)"
    );
}

/// #858 negative: a one-line result is kept whole, and a text with
/// nothing but noise and no ref update is empty, for the caller's own
/// word, never a stray `M\tpath` or `Your branch` line.
#[test]
fn result_line_keeps_a_lone_result_and_is_empty_for_pure_noise() {
    assert_eq!(result_line("Already up to date."), "Already up to date.");
    assert_eq!(
        result_line("Everything up-to-date"),
        "Everything up-to-date"
    );
    assert_eq!(
        result_line("branch 'newb' set up to track 'origin/newb'."),
        "branch 'newb' set up to track 'origin/newb'."
    );
    assert_eq!(
        result_line("M\tseed.txt\nYour branch is up to date with 'origin/main'."),
        ""
    );
    assert_eq!(result_line(""), "");
}
use super::*;
use crate::test_git::test_git_command;
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
fn porcelain_z_count_empty_means_clean() {
    assert_eq!(porcelain_z_count(b""), 0);
    assert_eq!(porcelain_z_count(b"\0"), 0);
}

#[test]
fn porcelain_z_count_counts_a_rename_once() {
    assert_eq!(porcelain_z_count(b" M src/app.rs\0"), 1);
    assert_eq!(porcelain_z_count(b"MM Cargo.toml\0 M src/lib.rs\0"), 2);
    assert_eq!(
        porcelain_z_count(b"R  new.rs\0old.rs\0?? x.txt\0"),
        2,
        "a rename's source record is not a second change"
    );
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
        let ok = test_git_command()
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
        let ok = test_git_command()
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
    let out = test_git_command()
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
        let out = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    let out = git_raw(p, &["check-ignore", "-z", "--stdin"], Some(&payload)).unwrap_or_default();
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
        test_git_command()
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
    let out = "12\t3\tsrc/app.rs\x005\t0\tREADME.md\x00-\t-\tassets/logo.png\x00";
    let map = parse_numstat(out);
    assert_eq!(map.get("src/app.rs"), Some(&(12, 3)));
    assert_eq!(map.get("README.md"), Some(&(5, 0)));
    assert_eq!(map.get("assets/logo.png"), Some(&(0, 0)));
    // `-z` paths are never quoted: a space or a quote is the name itself.
    let map = parse_numstat("1\t0\tmy \"notes\".md\x00");
    assert_eq!(map.get("my \"notes\".md"), Some(&(1, 0)));
}

#[test]
fn parse_numstat_keys_renames_on_destination_path() {
    // What `git diff --cached --numstat -z` prints for a rename into
    // another folder, a rename beside a plain edit, and a rename
    // inside one folder (#1164).
    let out = "1\t1\t\x00dir1/a.txt\x00dir2/a.txt\x00\
                   1\t1\tplain.txt\x00\
                   2\t1\t\x00src/old.rs\x00src/new.rs\x00";
    let map = parse_numstat(out);
    assert_eq!(map.get("dir2/a.txt"), Some(&(1, 1)));
    assert_eq!(map.get("plain.txt"), Some(&(1, 1)));
    assert_eq!(map.get("src/new.rs"), Some(&(2, 1)));
    assert_eq!(map.len(), 3, "{map:?}");
}

#[test]
fn a_staged_rename_counts_the_lines_it_changed() {
    // #1164: a renamed file that was also edited reported 0/0, so the
    // staged total undercounted the commit.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/old.rs"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
    sh_git(p, &["add", "."]);
    sh_git(p, &["commit", "-qm", "add"]);
    sh_git(p, &["mv", "src/old.rs", "src/new.rs"]);
    std::fs::write(p.join("src/new.rs"), "EDITED\nl2\nl3\nl4\nl5\n").unwrap();
    sh_git(p, &["add", "-A"]);
    let entries = query_changes(p);
    let renamed = entries
        .iter()
        .find(|e| e.path == "src/new.rs")
        .expect("the rename is listed under its new name");
    assert_eq!((renamed.additions, renamed.deletions), (1, 1));
}

#[test]
fn change_kind_section_groups_correctly() {
    assert_eq!(ChangeKind::StagedAdded.section(), ChangeSection::Staged);
    assert_eq!(ChangeKind::Modified.section(), ChangeSection::Changes);
    assert_eq!(ChangeKind::Untracked.section(), ChangeSection::Untracked);
    assert_eq!(ChangeKind::Conflicted.section(), ChangeSection::Conflicts);
}

#[test]
fn commit_rejects_empty_message() {
    let tmp = TempDir::new().unwrap();
    assert!(commit(tmp.path(), "").is_err());
    assert!(commit(tmp.path(), "   \n").is_err());
}

/// GIT_TERMINAL_PROMPT and a null stdin are checked too, but the session
/// is what tells: a test runner has no terminal to hand on and may set
/// GIT_TERMINAL_PROMPT=0 itself, while a hook still in croft's session
/// can open croft's `/dev/tty`.
#[cfg(target_os = "linux")]
#[test]
fn commit_never_offers_its_hooks_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        test_git_command()
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
    commit(p, "two").unwrap();
    assert_eq!(
        std::fs::read_to_string(p.join("seen")).unwrap(),
        "prompt=0\nstdin=none\nsession=own\n"
    );
}

#[test]
fn commit_creates_a_commit_in_a_real_repo() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(p.join("hello.txt"), "hi").unwrap();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["add", "."])
        .status();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["commit", "-m", "init", "--quiet"])
        .status();
    std::fs::write(p.join("hello.txt"), "hi v2").unwrap();
    let summary = commit(p, "second commit").expect("commit should succeed");
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
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = test_git_command()
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
    let _ = test_git_command()
        .args(["-C"])
        .arg(p_repo)
        .args(["init", "-q", "-b", "feature-x"])
        .output();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p_repo)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = test_git_command()
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
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = test_git_command()
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
    let out = test_git_command()
        .arg("-C")
        .arg(p)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What `git <args>` prints in `p` when run as the app runs git, to
/// compare with the app's own output: both read the developer's git
/// config, so a setting there (diff prefixes, `core.abbrev`, a log
/// format) changes both alike. A `diff` or `show` also drops colour and
/// an external diff tool, as the app's `diff_text` always does (#1447),
/// so a `color.diff = always` or a `diff.external` there leaves the two
/// sides equal.
fn app_git_out(p: &Path, args: &[&str]) -> String {
    let mut git = crate::test_git::app_git(p);
    match args.split_first() {
        Some((sub, rest)) if matches!(*sub, "diff" | "show") => git
            .arg(sub)
            .args(["--no-color", "--no-ext-diff"])
            .args(rest),
        _ => git.args(args),
    };
    let out = git.output().unwrap();
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
        let out = test_git_command()
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
        let _ = test_git_command()
            .arg("-C")
            .arg(tmp.path())
            .args(["remote", "add", name, url])
            .status();
    }
    assert_eq!(remote_for_slug(tmp.path(), "owner/repo"), "upstream");
    assert_eq!(remote_for_slug(tmp.path(), "me/fork"), "origin");
}

fn init_repo_with_commit(p: &Path) {
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(p.join("seed.txt"), "one\ntwo\n").unwrap();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["add", "."])
        .status();
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["commit", "-m", "init", "--quiet"])
        .status();
}

/// Pin git's own `a/` and `b/` diff prefixes in `p`'s repository config,
/// which outranks the developer's: a test that looks for them in the
/// app's diff output then holds whatever `diff.noprefix` or
/// `diff.mnemonicPrefix` the developer set. The app reads either form
/// (#2020); only the test's expected text names one.
fn pin_diff_prefixes(p: &Path) {
    git_in(p, &["config", "diff.noprefix", "false"]);
    git_in(p, &["config", "diff.mnemonicPrefix", "false"]);
}

/// `git -C p <args>`, asserting it succeeded; its stdout, trimmed.
fn git_in(p: &Path, args: &[&str]) -> String {
    let out = test_git_command()
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
    let head = test_git_command()
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

/// #1582: a message makes an annotated tag; without one (or a blank
/// one) the tag is lightweight, as before.
#[test]
fn create_tag_with_a_message_is_annotated() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    create_tag(p, "v1", Some("Release 1")).unwrap();
    assert_eq!(git_in(p, &["cat-file", "-t", "v1"]), "tag");
    assert_eq!(
        git_in(p, &["tag", "-l", "--format=%(contents:subject)", "v1"]),
        "Release 1"
    );
    create_tag(p, "v0", None).unwrap();
    create_tag(p, "v0b", Some("   ")).unwrap();
    assert_eq!(git_in(p, &["cat-file", "-t", "v0"]), "commit");
    assert_eq!(git_in(p, &["cat-file", "-t", "v0b"]), "commit");
}

/// #1582: Push Tags sends local tags to the remote, the remote's tags
/// list by name, and Delete Remote Tag removes one there only.
#[test]
fn tags_push_list_and_delete_on_the_remote() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    let remote = TempDir::new().unwrap();
    git_in(remote.path(), &["init", "-q", "--bare"]);
    git_in(
        p,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git_in(p, &["tag", "v1"]);
    git_in(p, &["tag", "-a", "v2", "-m", "two"]);
    assert_eq!(tag_remote(p).as_deref(), Ok("origin"));
    assert_eq!(list_remote_tags(p, "origin"), Ok(Vec::new()));
    push_tags(p, "origin").unwrap();
    assert_eq!(
        list_remote_tags(p, "origin"),
        Ok(vec![String::from("v1"), String::from("v2")])
    );
    delete_remote_tag(p, "origin", "v1").unwrap();
    assert_eq!(list_remote_tags(p, "origin"), Ok(vec![String::from("v2")]));
    assert_eq!(git_in(p, &["tag"]), "v1\nv2", "local tags stay");
}

/// #1582, negative: no remote, or several with none of them origin and
/// no `remote.pushDefault`, is no remote to guess; `pushDefault` wins.
/// ls-remote's peeled and non-tag lines are not tags.
#[test]
fn the_tag_remote_is_never_a_guess() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    assert_eq!(
        tag_remote(p),
        Err(String::from("no remote to push tags to"))
    );
    git_in(p, &["remote", "add", "a", "/nowhere/a"]);
    git_in(p, &["remote", "add", "b", "/nowhere/b"]);
    assert!(tag_remote(p).unwrap_err().contains("remote.pushDefault"));
    git_in(p, &["config", "remote.pushDefault", "b"]);
    assert_eq!(tag_remote(p).as_deref(), Ok("b"));
    assert_eq!(
        parse_remote_tags("abc\trefs/tags/v1\ndef\trefs/heads/main\nfff\trefs/tags/v2^{}\nnoise"),
        ["v1"]
    );
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

/// #1513: each porcelain status maps to its Explorer letter, a partially
/// staged file keeps its strongest status, and each folder above a
/// change takes the strongest status beneath it, whatever order the
/// lines come in. The root itself is never decorated.
#[test]
fn decorations_carry_each_status_and_roll_up_to_folders() {
    use GitDecoration as D;
    let root = Path::new("/w");
    let porcelain = b" M a.py\0?? c.py\0A  d.py\0R  new.py\0old.py\0UU x.py\0 D gone.py\0AM part.py\0?? pkg/new.py\0 M pkg/old.py\0?? pkg/deep/n.py\0 M lib/x/y.py\0?? lib/z.py\0";
    let got = parse_decorations(root, "", porcelain);
    let at = |p: &str| got.get(&root.join(p)).copied();
    assert_eq!(at("a.py"), Some(D::Modified));
    assert_eq!(at("c.py"), Some(D::Untracked));
    assert_eq!(at("d.py"), Some(D::Added));
    assert_eq!(at("new.py"), Some(D::Renamed));
    assert_eq!(at("old.py"), None, "a rename decorates its destination");
    assert_eq!(at("x.py"), Some(D::Conflicted));
    assert_eq!(at("gone.py"), Some(D::Deleted));
    assert_eq!(
        at("part.py"),
        Some(D::Modified),
        "AM: the strongest of the two"
    );
    assert_eq!(
        at("pkg"),
        Some(D::Modified),
        "untracked first, modified wins"
    );
    assert_eq!(at("pkg/deep"), Some(D::Untracked));
    assert_eq!(
        at("lib"),
        Some(D::Modified),
        "modified first, untracked does not demote"
    );
    assert_eq!(at("lib/x"), Some(D::Modified));
    assert_eq!(got.get(root), None, "the root row is never decorated");
    let letters: String = [
        D::Untracked,
        D::Added,
        D::Renamed,
        D::Deleted,
        D::Modified,
        D::Conflicted,
    ]
    .map(GitDecoration::letter)
    .iter()
    .collect();
    assert_eq!(letters, "UARDM!");
}

/// #1513 negatives: a workspace root inside the repository keeps only
/// its own changes (porcelain paths are toplevel-relative), and a clean
/// tree has no decorations.
#[test]
fn decorations_stay_inside_the_workspace_root() {
    let root = Path::new("/repo/app");
    let got = parse_decorations(
        root,
        "app/\n",
        b" M app/src/x.py\0 M other/y.py\0?? apple.py\0",
    );
    assert_eq!(
        got.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from([root.join("src/x.py"), root.join("src")])
    );
    assert!(parse_decorations(Path::new("/w"), "", b"").is_empty());
}

/// #1513: a decoration keys the exact path bytes, so a filename that is
/// not UTF-8 and a rename whose source contains ` -> ` still mark the
/// file the tree holds.
#[cfg(unix)]
#[test]
fn decorations_keep_exact_path_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let root = Path::new("/w");
    let got = parse_decorations(
        root,
        "",
        b" M bad\xff.py\0R  new.py\0a -> b.py\0?? sp ace.py\0",
    );
    assert_eq!(
        got.get(&root.join(OsStr::from_bytes(b"bad\xff.py"))),
        Some(&GitDecoration::Modified)
    );
    assert_eq!(got.get(&root.join("new.py")), Some(&GitDecoration::Renamed));
    assert_eq!(
        got.get(&root.join("sp ace.py")),
        Some(&GitDecoration::Untracked)
    );
    assert_eq!(got.len(), 3, "the rename source is not decorated: {got:?}");
}

/// #1513, end to end over the issue's repro: modified, staged and
/// untracked files are decorated from a real `git status`, and the
/// unchanged file is not.
#[test]
fn query_decorates_changed_files_and_their_folders() {
    use GitDecoration as D;
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    std::fs::create_dir_all(p.join("sub")).unwrap();
    for f in ["a.py", "b.py", "sub/e.py"] {
        std::fs::write(p.join(f), "x = 1\n").unwrap();
    }
    init_repo_with_commit(p);
    std::fs::write(p.join("a.py"), "x = 1\ny = 2\n").unwrap();
    std::fs::write(p.join("sub/e.py"), "x = 1\ny = 2\n").unwrap();
    std::fs::write(p.join("c.py"), "new = 1\n").unwrap();
    std::fs::write(p.join("d.py"), "staged = 1\n").unwrap();
    test_git_command()
        .arg("-C")
        .arg(p)
        .args(["add", "d.py"])
        .status()
        .unwrap();
    let st = query(p);
    let at = |f: &str| st.decorations.get(&p.join(f)).copied();
    assert_eq!(at("a.py"), Some(D::Modified));
    assert_eq!(at("sub/e.py"), Some(D::Modified));
    assert_eq!(at("sub"), Some(D::Modified));
    assert_eq!(at("c.py"), Some(D::Untracked));
    assert_eq!(at("d.py"), Some(D::Added));
    assert_eq!(at("b.py"), None, "an unchanged file is not decorated");
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

/// #1620: `commit.template` fills the message, comments dropped, read
/// from the repository's top level even when croft is in a subfolder;
/// a merge's own message wins over it.
#[test]
fn the_commit_template_is_the_message_when_nothing_is_prepared() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(
        p.join(".gitmessage"),
        "PROJ-: \n\nWhy:\n\n# Subject: PROJ-<ticket>: <summary>, at most 50 characters\n",
    )
    .unwrap();
    run_git(p, &["config", "commit.template", ".gitmessage"]).unwrap();
    assert_eq!(query(p).prepared_message.as_deref(), Some("PROJ-:\n\nWhy:"));
    std::fs::create_dir(p.join("sub")).unwrap();
    assert_eq!(
        query(&p.join("sub")).prepared_message.as_deref(),
        Some("PROJ-:\n\nWhy:"),
        "relative to the top level, not the folder croft is in"
    );
    std::fs::write(p.join(".git/MERGE_MSG"), "Merge branch 'other'\n").unwrap();
    assert_eq!(
        query(p).prepared_message.as_deref(),
        Some("Merge branch 'other'")
    );
}

/// #1620: with `core.commentChar` set, lines starting with it are the
/// comments, and `#` lines are message text.
#[test]
fn the_configured_comment_char_marks_template_comments() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join(".gitmessage"), "#42: \n; helper text\n").unwrap();
    run_git(p, &["config", "commit.template", ".gitmessage"]).unwrap();
    run_git(p, &["config", "core.commentChar", ";"]).unwrap();
    assert_eq!(query(p).prepared_message.as_deref(), Some("#42:"));
}

/// Negative (#1620): a template that names no readable file, or holds
/// only comments, is no message; an absolute path is read as named.
#[test]
fn a_missing_or_comment_only_template_is_no_message() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    run_git(p, &["config", "commit.template", "nope.txt"]).unwrap();
    assert_eq!(query(p).prepared_message, None);
    let only = tmp.path().join("only-comments.txt");
    std::fs::write(&only, "# write something\n\n").unwrap();
    run_git(p, &["config", "commit.template", only.to_str().unwrap()]).unwrap();
    assert_eq!(query(p).prepared_message, None);
    let abs = tmp.path().join("abs.txt");
    std::fs::write(&abs, "Ticket: \n").unwrap();
    run_git(p, &["config", "commit.template", abs.to_str().unwrap()]).unwrap();
    assert_eq!(query(p).prepared_message.as_deref(), Some("Ticket:"));
}

/// #1620: under `core.commentChar=auto` git picks a marker no line of
/// the template starts with, so every line of it is the message.
#[test]
fn under_an_auto_comment_char_every_template_line_is_kept() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join(".gitmessage"), "#42: \n; helper text\n").unwrap();
    run_git(p, &["config", "commit.template", ".gitmessage"]).unwrap();
    run_git(p, &["config", "core.commentChar", "auto"]).unwrap();
    assert_eq!(
        query(p).prepared_message.as_deref(),
        Some("#42:\n; helper text")
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
        let _ = test_git_command().args(["-C"]).arg(p).args(args).status();
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
        let _ = test_git_command()
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
        let _ = test_git_command()
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

    let _ = test_git_command()
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
        let _ = test_git_command().args(["-C"]).arg(p).args(args).status();
    };
    // A sibling branch whose commit dates FUTURE, so any date- or
    // topo-ordered listing across all branches puts it first.
    git(&["checkout", "-q", "-b", "sibling"]);
    std::fs::write(p.join("sibling.txt"), "s").unwrap();
    git(&["add", "-A"]);
    let _ = test_git_command()
        .args(["-C"])
        .arg(p)
        .args(["commit", "-q", "-m", "sibling-future"])
        .env("GIT_COMMITTER_DATE", "2038-01-01T00:00:00")
        .env("GIT_AUTHOR_DATE", "2038-01-01T00:00:00")
        .status();
    // Back to the original branch.
    git(&["checkout", "-q", "-"]);

    let head = test_git_command()
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
        let _ = test_git_command().args(["-C"]).arg(p).args(args).status();
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
        test_git_command()
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
    let out = test_git_command()
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
    let out = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    let _ = test_git_command()
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

/// #1447 fixture: a repo with `app.py` committed and an edit staged.
fn repo_with_a_staged_edit() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    let head = sh_git(p, &["rev-parse", "HEAD"]);
    std::fs::write(p.join("seed.txt"), "one\nTWO\n").unwrap();
    sh_git(p, &["add", "seed.txt"]);
    (tmp, head.trim().to_string())
}

/// #1447: `color.ui = always` (here in the repo's own config, which
/// git reads the same way as `~/.gitconfig`) never reaches the text
/// croft parses as a patch.
#[test]
fn git_diff_views_ignore_a_color_always_config() {
    let (tmp, head) = repo_with_a_staged_edit();
    let p = tmp.path();
    sh_git(p, &["config", "color.ui", "always"]);
    sh_git(p, &["config", "color.diff", "always"]);
    let coloured = sh_git(p, &["diff", "--staged"]);
    assert!(
        coloured.contains('\x1b'),
        "the config colours git's own output: {coloured:?}"
    );
    let staged = diff_staged(p).unwrap();
    assert!(staged.starts_with("diff --git"), "{staged:?}");
    assert!(!staged.contains('\x1b'), "{staged:?}");
    assert!(staged.contains("\n+TWO\n"), "{staged:?}");
    let previous = diff_against_branch(p, &head).unwrap();
    assert!(!previous.contains('\x1b'), "{previous:?}");
    sh_git(p, &["commit", "-qm", "two"]);
    let commit = show_commit(p, "HEAD").unwrap();
    assert!(commit.starts_with("commit "), "{commit:?}");
    assert!(!commit.contains('\x1b'), "{commit:?}");
    let file = show_commit_file_diff(p, "HEAD", "seed.txt").unwrap();
    assert!(!file.contains('\x1b'), "{file:?}");
    assert!(!diff_previous_commit(p).unwrap().contains('\x1b'));
}

/// #1447: a `diff.external` tool (difftastic's recommended setup) does
/// not replace the patch with its own output.
#[test]
fn git_diff_views_ignore_an_external_diff_tool() {
    let (tmp, _) = repo_with_a_staged_edit();
    let p = tmp.path();
    let tool = p.join("external-diff.sh");
    std::fs::write(&tool, "#!/bin/sh\necho EXTERNAL\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    sh_git(p, &["config", "diff.external", tool.to_str().unwrap()]);
    assert!(sh_git(p, &["diff", "--staged"]).contains("EXTERNAL"));
    let staged = diff_staged(p).unwrap();
    assert!(staged.starts_with("diff --git"), "{staged:?}");
    assert!(!staged.contains("EXTERNAL"), "{staged:?}");
}

/// #1447 negative: with git's default colour settings the text is
/// exactly what plain `git diff` / `git show` print.
#[test]
fn git_diff_views_are_unchanged_without_colour_config() {
    let (tmp, _) = repo_with_a_staged_edit();
    let p = tmp.path();
    assert_eq!(
        diff_staged(p).unwrap(),
        app_git_out(p, &["diff", "--staged"])
    );
    sh_git(p, &["commit", "-qm", "two"]);
    assert_eq!(
        show_commit(p, "HEAD").unwrap(),
        app_git_out(p, &["show", "--stat", "--patch", "HEAD"])
    );
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

/// #1444 fixture: `feature` branched from `main` and added
/// `feature.py`; `main` then got a teammate's `teammate.py` and an
/// `app.py` change. HEAD is `feature`.
fn branch_behind_a_moved_main() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        let o = test_git_command()
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
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(p.join("app.py"), "def a():\n    return 1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    git(&["checkout", "-qb", "feature"]);
    std::fs::write(p.join("feature.py"), "def feature():\n    return 2\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "add feature"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(p.join("teammate.py"), "def teammate():\n    return 3\n").unwrap();
    std::fs::write(p.join("app.py"), "def a():\n    return 10\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "teammate work"]);
    git(&["checkout", "-q", "feature"]);
    tmp
}

/// #1444: the view holds the branch's own changes only, as a pull
/// request diff does, not main's later work shown in reverse.
#[test]
fn diff_against_branch_starts_from_the_merge_base() {
    let tmp = branch_behind_a_moved_main();
    pin_diff_prefixes(tmp.path());
    let out = diff_against_branch(tmp.path(), "main").unwrap();
    assert!(out.contains("+++ b/feature.py"), "{out}");
    assert!(!out.contains("teammate.py"), "{out}");
    assert!(!out.contains("app.py"), "{out}");
}

/// #1444: uncommitted work on the branch is still in the view.
#[test]
fn diff_against_branch_keeps_uncommitted_edits() {
    let tmp = branch_behind_a_moved_main();
    std::fs::write(
        tmp.path().join("feature.py"),
        "def feature():\n    return 22\n",
    )
    .unwrap();
    let out = diff_against_branch(tmp.path(), "main").unwrap();
    assert!(out.contains("+    return 22"), "{out}");
    assert!(!out.contains("teammate.py"), "{out}");
}

/// #1444 negative: when main has not moved, the view is exactly
/// `git diff main`, as before.
#[test]
fn diff_against_an_unmoved_branch_is_the_plain_diff() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    let git = |args: &[&str]| {
        test_git_command()
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap()
    };
    git(&["checkout", "-qb", "feature"]);
    std::fs::write(p.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["commit", "-qam", "three"]);
    std::fs::write(p.join("new.txt"), "x\n").unwrap();
    git(&["add", "new.txt"]);
    let plain = app_git_out(p, &["diff", "main"]);
    assert!(plain.contains("+three"), "{plain}");
    assert_eq!(diff_against_branch(p, "main").unwrap(), plain);
}

/// #1444 negative: with no shared history there is no merge base, and
/// the view falls back to the tip diff instead of failing.
#[test]
fn diff_against_an_unrelated_branch_falls_back_to_its_tip() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    let git = |args: &[&str]| {
        test_git_command()
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap()
    };
    git(&["checkout", "-q", "--orphan", "other"]);
    git(&["rm", "-rfq", "."]);
    std::fs::write(p.join("other.txt"), "other\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "other root"]);
    assert_eq!(merge_base(p, "main"), None);
    let plain = app_git_out(p, &["diff", "main"]);
    assert!(plain.contains("seed.txt"), "{plain}");
    assert_eq!(diff_against_branch(p, "main").unwrap(), plain);
}

/// #1444 negative: a branch git does not know still errors, as before.
#[test]
fn diff_against_an_unknown_branch_still_errors() {
    let tmp = TempDir::new().unwrap();
    init_repo_with_commit(tmp.path());
    assert!(diff_against_branch(tmp.path(), "no-such-branch").is_err());
}

#[test]
fn a_commit_from_before_a_rename_shows_its_diff_under_the_old_name() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        let o = test_git_command()
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
fn follow_names_lists_the_old_name_before_a_rename() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        assert!(
            test_git_command()
                .arg("-C")
                .arg(p)
                .args(args)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?}"
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::create_dir(p.join("sub")).unwrap();
    std::fs::write(p.join("sub/old.py"), "a = 1\nb = 2\nc = 3\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "add"]);
    git(&["mv", "sub/old.py", "sub/new.py"]);
    git(&["commit", "-q", "-m", "rename"]);
    let names: Vec<String> = follow_names(p, "sub/new.py", 10)
        .into_iter()
        .map(|(hash, path)| {
            assert_eq!(hash.len(), 40, "full hashes, as the scrubber keys them");
            path
        })
        .collect();
    assert_eq!(names, ["sub/new.py", "sub/old.py"]);
    // From a workspace rooted in the subfolder, paths are relative to it.
    let names: Vec<String> = follow_names(&p.join("sub"), "new.py", 10)
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    assert_eq!(names, ["new.py", "old.py"]);
    assert!(follow_names(p, "missing.py", 10).is_empty());
}

/// #1304: a rename merged from a side branch is listed on the merge,
/// one of the first-parent commits the scrubber walks. A full walk
/// went down the side branch and skipped the merge, so the scrubber
/// read the merge under the old name, which did not exist there.
#[test]
fn follow_names_lists_a_rename_merged_from_a_side_branch_on_the_merge() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        let out = test_git_command()
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(p.join("old.py"), "a = 1\nb = 2\nc = 3\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "add"]);
    let added = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "-b", "side"]);
    git(&["mv", "old.py", "new.py"]);
    git(&["commit", "-q", "-m", "rename"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(p.join("other.txt"), "x\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "other"]);
    git(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    let merge = git(&["rev-parse", "HEAD"]);
    assert_eq!(
        follow_names(p, "new.py", 10),
        [(merge, "new.py".to_string()), (added, "old.py".to_string())]
    );
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
    let (first, text) = b.line(0).unwrap();
    assert_eq!(first.hash, "abcdef0123456789abcdef0123456789abcdef01");
    assert_eq!(first.short_hash(), "abcdef01");
    assert_eq!(first.author, "Vitali");
    assert_eq!(first.summary, "fix: the thing");
    assert_eq!(first.age_secs, 4, "now (10000) minus author-time (9996)");
    assert!(!first.uncommitted);
    assert_eq!(text, Some("one"));
    assert!(
        b.line(1).unwrap().0.uncommitted,
        "the zero hash marks an uncommitted line"
    );
}

/// `--porcelain` describes a commit only the first time it appears
/// (#1707): every later line of it must still read its author, age and
/// summary, from the one record they share.
#[test]
fn parse_blame_gives_a_commits_later_lines_its_first_description() {
    let now = 10_000i64;
    let out = "\
abcdef0123456789abcdef0123456789abcdef01 1 1 1
author Vitali
author-time 9996
summary fix: the thing
filename seed.txt
\tone
1111111111111111111111111111111111111111 2 2 1
author Alice
author-time 9000
summary add two
filename seed.txt
\ttwo
abcdef0123456789abcdef0123456789abcdef01 3 3 1
filename seed.txt
\tthree
";
    let b = parse_blame(out, now);
    assert_eq!(b.len(), 3, "one entry per source line");
    let (third, text) = b.line(2).unwrap();
    assert_eq!(text, Some("three"));
    assert_eq!(third.author, "Vitali");
    assert_eq!(third.summary, "fix: the thing");
    assert_eq!(third.age_secs, 4);
    assert_eq!(third.short_hash(), "abcdef01");
    assert!(
        std::ptr::eq(b.line(0).unwrap().0, third),
        "two lines of one commit share its record"
    );
    assert_eq!(b.line(1).unwrap().0.author, "Alice");
}

#[test]
fn blame_returns_one_entry_per_committed_line() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    // seed.txt is "one\ntwo\n" — two committed lines.
    let b = blame(p, "seed.txt");
    assert_eq!(b.len(), 2, "blame reports every source line");
    let (first, _) = b.line(0).unwrap();
    assert_eq!(
        first.author, "a",
        "committed by init_repo_with_commit's user"
    );
    assert!(!first.uncommitted);
    assert_eq!(first.hash.len(), 40, "the full hash");
    assert_eq!(
        b.line(1).unwrap().0.author,
        "a",
        "a commit's second line reads its author too"
    );
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

/// #1556: a TIMELINE row or Git: Open Commit for Line in a workspace
/// opened above the repository must still show the commit's diff.
#[test]
fn a_commits_file_diff_comes_from_the_repo_below_the_workspace_root() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    init_repo_with_commit(&sub);
    pin_diff_prefixes(&sub);
    let raw = show_commit_file_diff(tmp.path(), "HEAD", "sub/seed.txt").unwrap();
    assert!(raw.contains("diff --git a/seed.txt b/seed.txt"), "{raw}");
    assert!(raw.lines().any(|l| l == "+two"), "{raw}");
}

#[test]
fn blame_finds_the_repo_below_the_workspace_root() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    init_repo_with_commit(&sub);
    let b = blame(tmp.path(), "sub/seed.txt");
    assert_eq!(b.len(), 2, "blame of sub/seed.txt from the parent");
    assert!(!b.line(0).unwrap().0.uncommitted);
}

#[test]
fn file_history_of_a_file_in_a_repo_subfolder_still_resolves() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::create_dir(p.join("dir")).unwrap();
    std::fs::write(p.join("dir/x.txt"), "x\n").unwrap();
    let _ = test_git_command()
        .arg("-C")
        .arg(p)
        .args(["add", "."])
        .status();
    let _ = test_git_command()
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
    let _ = test_git_command()
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
    stash_push(p, None).expect("stash push must succeed");
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

/// A committed repo with `seed.txt` and `wip.txt` both edited.
fn repo_with_two_edits(p: &Path) {
    init_repo_with_commit(p);
    std::fs::write(p.join("wip.txt"), "draft\n").unwrap();
    git_in(p, &["add", "wip.txt"]);
    git_in(p, &["commit", "-qm", "add wip"]);
    std::fs::write(p.join("seed.txt"), "one\ntwo\nready\n").unwrap();
    std::fs::write(p.join("wip.txt"), "draft\nnot ready\n").unwrap();
}

#[test]
fn commit_takes_only_the_index_when_anything_is_staged() {
    // #965: `git commit -am` swept the unstaged edit in with the staged one.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    repo_with_two_edits(p);
    stage(p, "seed.txt");
    commit(p, "ready part").expect("commit");
    assert_eq!(git_in(p, &["show", "HEAD:seed.txt"]), "one\ntwo\nready");
    assert_eq!(git_in(p, &["show", "HEAD:wip.txt"]), "draft");
    assert_eq!(
        std::fs::read_to_string(p.join("wip.txt")).unwrap(),
        "draft\nnot ready\n",
        "the unstaged edit stays in the working tree"
    );
    assert_eq!(git_in(p, &["diff", "--name-only"]), "wip.txt");
}

#[test]
fn commit_takes_every_tracked_change_when_nothing_is_staged() {
    // Negative: with an empty index it is still VS Code's smart commit.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    repo_with_two_edits(p);
    std::fs::write(p.join("new.txt"), "untracked\n").unwrap();
    commit(p, "everything").expect("commit");
    assert_eq!(git_in(p, &["show", "HEAD:seed.txt"]), "one\ntwo\nready");
    assert_eq!(git_in(p, &["show", "HEAD:wip.txt"]), "draft\nnot ready");
    assert_eq!(git_in(p, &["status", "--porcelain"]), "?? new.txt");
}

#[test]
fn a_subdirectory_root_with_diff_relative_still_sees_what_is_staged() {
    // `diff.relative` would scope the check to the subdirectory, and
    // `-a` would then sweep in the unstaged edit as well.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    repo_with_two_edits(p);
    git_in(p, &["config", "diff.relative", "true"]);
    stage(p, "seed.txt");
    let sub = p.join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(has_staged_changes(&sub), Ok(true));
    commit(&sub, "ready part").expect("commit");
    assert_eq!(git_in(p, &["show", "HEAD:wip.txt"]), "draft");
}

#[test]
fn a_staged_submodule_update_counts_despite_diff_ignore_submodules() {
    // `diff.ignoreSubmodules=all` hid the staged gitlink, so the commit
    // took `-a` and swept the unstaged edit in with it.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    repo_with_two_edits(p);
    git_in(p, &["config", "diff.ignoreSubmodules", "all"]);
    let head = git_in(p, &["rev-parse", "HEAD"]);
    git_in(
        p,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},lib"),
        ],
    );
    assert_eq!(has_staged_changes(p), Ok(true));
    commit(p, "bump lib").expect("commit");
    assert_eq!(git_in(p, &["show", "HEAD:wip.txt"]), "draft");
}

#[test]
fn a_staged_change_counts_despite_a_trusted_external_diff() {
    // From git 2.46 a `diff.external` whose exit code is trusted decides
    // what `--quiet` reports; one answering "equal" hid the staged edit,
    // so the commit took `-a` and swept the unstaged one in with it.
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    repo_with_two_edits(p);
    stage(p, "seed.txt");
    git_in(p, &["config", "diff.external", "true"]);
    git_in(p, &["config", "diff.trustExitCode", "true"]);
    assert_eq!(has_staged_changes(p), Ok(true));
    commit(p, "ready part").expect("commit");
    assert_eq!(git_in(p, &["show", "HEAD:wip.txt"]), "draft");
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
    create_tag(p, "v1.0.0", None).expect("create_tag");
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
    stash_push(p, None).expect("first stash");
    std::fs::write(p.join("seed.txt"), "one\ntwo\nsecond\n").unwrap();
    stash_push(p, None).expect("second stash");
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

// #1581: a stash takes the name it is given, for every kind of stash,
// and its row says how old it is and which files it holds.
#[test]
fn a_stash_message_names_the_stash_and_the_row_lists_its_files() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join("seed.txt"), "one\ntwo\ntracked\n").unwrap();
    stash_push(p, Some("retry tweak")).expect("named stash");
    std::fs::write(p.join("new.txt"), "untracked\n").unwrap();
    stash_push_untracked(p, Some("with the new file")).expect("named -u stash");
    std::fs::write(p.join("seed.txt"), "one\ntwo\nstaged\n").unwrap();
    run_git(p, &["add", "seed.txt"]).unwrap();
    stash_push_staged(p, Some("only staged")).expect("named --staged stash");
    let stashes = list_stashes(p).unwrap();
    let messages: Vec<&str> = stashes.iter().map(|s| s.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "On main: only staged",
            "On main: with the new file",
            "On main: retry tweak"
        ]
    );
    assert_eq!(stashes[1].files, ["new.txt"], "untracked files are listed");
    assert_eq!(stashes[2].files, ["seed.txt"]);
    assert!(!stashes[2].age.is_empty(), "{:?}", stashes[2]);
    let label = stashes[2].label();
    assert!(
        label.starts_with("stash@{2}: On main: retry tweak \u{b7} ")
            && label.ends_with(" \u{b7} seed.txt"),
        "{label}"
    );
}

// #1581: the file lists come from one listing for every stash, so a
// stash's tracked and untracked files are merged into one row and
// each stash keeps its own files.
#[test]
fn stash_rows_merge_tracked_and_untracked_files_per_stash() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join("seed.txt"), "one\ntwo\nmixed\n").unwrap();
    std::fs::write(p.join("a_new.txt"), "untracked\n").unwrap();
    stash_push_untracked(p, Some("mixed")).unwrap();
    std::fs::write(p.join("seed.txt"), "one\ntwo\nplain\n").unwrap();
    stash_push(p, Some("plain")).unwrap();
    std::fs::write(p.join("z_new.txt"), "only untracked\n").unwrap();
    stash_push_untracked(p, Some("untracked only")).unwrap();
    let stashes = list_stashes(p).unwrap();
    assert_eq!(stashes.len(), 3);
    assert_eq!(stashes[0].files, ["z_new.txt"]);
    assert_eq!(stashes[1].files, ["seed.txt"]);
    assert_eq!(stashes[2].files, ["a_new.txt", "seed.txt"]);
    assert_eq!(
        stashes.iter().map(|s| s.index).collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

// Negative (#1581): a blank or missing message leaves git's own
// `WIP on` name, and a row never lists more than three files.
#[test]
fn a_blank_stash_message_keeps_the_wip_name_and_long_file_lists_are_cut() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join("seed.txt"), "one\ntwo\nblank\n").unwrap();
    stash_push(p, Some("   ")).expect("blank-named stash");
    let stashes = list_stashes(p).unwrap();
    assert!(
        stashes[0].message.starts_with("WIP on main: "),
        "{:?}",
        stashes[0]
    );
    let info = StashInfo {
        index: 4,
        message: "On main: big".into(),
        age: String::new(),
        files: ["a", "b", "c", "d", "e"].map(String::from).to_vec(),
    };
    assert_eq!(info.label(), "stash@{4}: On main: big \u{b7} a, b, c +2");
}

// #1581: View Stash shows the stash's diff, untracked files included,
// without applying it.
#[test]
fn show_stash_is_the_patch_without_applying_it() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::write(p.join("seed.txt"), "one\ntwo\nretries = 5\n").unwrap();
    std::fs::write(p.join("extra.txt"), "brand new\n").unwrap();
    stash_push_untracked(p, Some("retries")).unwrap();
    let patch = show_stash(p, 0).expect("show_stash");
    assert!(patch.contains("+retries = 5"), "{patch}");
    assert!(patch.contains("+brand new"), "untracked: {patch}");
    assert!(patch.contains("2 files changed"), "the stat: {patch}");
    assert_eq!(
        std::fs::read_to_string(p.join("seed.txt")).unwrap(),
        "one\ntwo\n",
        "viewing does not apply"
    );
    assert_eq!(list_stashes(p).unwrap().len(), 1, "viewing does not drop");
    assert!(show_stash(p, 3).is_err(), "no such stash");
}

#[test]
fn pull_on_an_up_to_date_clone_succeeds() {
    let upstream = TempDir::new().unwrap();
    let up = upstream.path();
    init_repo_with_commit(up);
    let clone = TempDir::new().unwrap();
    let clone_path = clone.path().join("work");
    let ok = test_git_command()
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

/// #989: the issue's repro, a merge stopped on a conflict in README.md.
fn repo_in_a_conflicted_merge() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let script = r#"set -e
git init -q -b main && git config user.email a@b && git config user.name a
printf '# Orders\n\nRun `python pricing.py`.\n' > README.md && git add . && git commit -qm init
git checkout -qb hotfix && sed 's/python pricing.py/python -m pricing/' README.md > README.tmp && mv README.tmp README.md && git commit -qam fix
git checkout -q main && sed 's/python pricing.py/uv run pricing.py/' README.md > README.tmp && mv README.tmp README.md && git commit -qam uv
! git merge -q hotfix >/dev/null 2>&1"#;
    let out = Command::new("sh")
        .args(["-c", script])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    tmp
}

fn head_of(p: &Path) -> String {
    let out = test_git_command()
        .arg("-C")
        .arg(p)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn commit_refuses_while_a_merge_has_unresolved_conflicts() {
    // `git commit -a` stages an unmerged file as it is, so the merge
    // commit recorded the conflict markers.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    let before = head_of(p);
    let err = commit(p, "Merge hotfix").expect_err("the commit must be refused");
    assert!(
        err.contains("Resolve 1 merge conflict (README.md)"),
        "{err}"
    );
    assert_eq!(head_of(p), before, "no commit was made");
    assert_eq!(unmerged_paths(p), vec![String::from("README.md")]);
    assert!(
        p.join(".git/MERGE_HEAD").exists(),
        "the merge is still open"
    );
}

#[test]
fn a_merge_whose_conflicts_are_resolved_and_staged_still_commits() {
    // Negative: the guard only holds back unresolved paths.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    std::fs::write(
        p.join("README.md"),
        "# Orders\n\nRun `uv run -m pricing`.\n",
    )
    .unwrap();
    run_mutation(p, &["add", "README.md"]).unwrap();
    assert!(unmerged_paths(p).is_empty());
    commit(p, "Merge hotfix").expect("a resolved merge commits");
    let parents = test_git_command()
        .arg("-C")
        .arg(p)
        .args(["rev-list", "--parents", "-n1", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        3,
        "a merge commit with two parents"
    );
}

#[test]
fn the_commits_that_do_not_stage_are_refused_by_git_itself() {
    // Negative: Commit Staged and Amend run a plain `git commit`, which
    // refuses an unmerged index on its own, so they need no guard.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    let before = head_of(p);
    assert!(commit_staged(p, "Merge hotfix").is_err());
    assert!(commit_amend(p, "amended").is_err());
    assert!(commit_amend_no_edit(p).is_err());
    assert_eq!(head_of(p), before);
}

#[test]
fn a_subdirectory_root_with_diff_relative_still_sees_the_conflicts() {
    // `diff.relative` scoped the check to the subdirectory, so a
    // conflict outside it slipped past the guard.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    run_mutation(p, &["config", "diff.relative", "true"]).unwrap();
    let sub = p.join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(unmerged_paths(&sub), vec![String::from("README.md")]);
    assert!(refuse_unmerged(&sub).is_err());
}

#[test]
fn staging_a_conflict_that_still_has_its_markers_is_refused() {
    // `git add` marks the conflict resolved with the markers in it, so
    // Stage All, then Commit, recorded them.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    let err = stage_all(p).expect_err("Stage All must refuse");
    assert_eq!(
        err,
        "README.md still has conflict markers: resolve it before staging"
    );
    let err = stage_path(p, "README.md").expect_err("staging the file must refuse");
    assert!(
        err.contains("README.md still has conflict markers"),
        "{err}"
    );
    assert_eq!(unmerged_paths(p), vec![String::from("README.md")]);
}

#[test]
fn a_conflict_in_a_non_utf8_file_is_refused_too() {
    // A Latin-1 file can't be read as UTF-8; its markers still count.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    std::fs::write(
        p.join("README.md"),
        b"# Caf\xe9\n<<<<<<< HEAD\nuv\n=======\npython -m\n>>>>>>> hotfix\n",
    )
    .unwrap();
    assert!(stage_all(p).is_err());
    assert_eq!(unmerged_paths(p), vec![String::from("README.md")]);
}

#[test]
fn markers_of_a_conflict_marker_size_attribute_are_refused_too() {
    // git writes ten-character markers for this file, which a scan for
    // seven-character ones read as resolved.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    std::fs::write(
        p.join(".gitattributes"),
        "README.md conflict-marker-size=10\n",
    )
    .unwrap();
    run_mutation(p, &["checkout", "--conflict=merge", "--", "README.md"]).unwrap();
    let text = std::fs::read_to_string(p.join("README.md")).unwrap();
    assert!(text.contains("<<<<<<<<<< ours"), "{text}");
    assert!(stage_path(p, "README.md").is_err());
}

#[test]
fn a_resolved_file_that_shows_a_marker_example_both_sides_had_stages() {
    // Negative: a doc that shows what a conflict looks like keeps that
    // block after its real conflict is resolved; it must not block
    // staging forever.
    let tmp = TempDir::new().unwrap();
    let script = r#"set -e
git init -q -b main && git config user.email a@b && git config user.name a
printf 'Example:\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> br\n\nRun `python pricing.py`.\n' > GUIDE.md && git add . && git commit -qm init
git checkout -qb hotfix && sed 's/python pricing.py/python -m pricing/' GUIDE.md > g.tmp && mv g.tmp GUIDE.md && git commit -qam fix
git checkout -q main && sed 's/python pricing.py/uv run pricing.py/' GUIDE.md > g.tmp && mv g.tmp GUIDE.md && git commit -qam uv
! git merge -q hotfix >/dev/null 2>&1
sed -n '1,7p' GUIDE.md > g.tmp && echo 'Run `uv run -m pricing`.' >> g.tmp && mv g.tmp GUIDE.md"#;
    let out = Command::new("sh")
        .args(["-c", script])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = tmp.path();
    assert_eq!(unmerged_paths(p), vec![String::from("GUIDE.md")]);
    stage_all(p).expect("the example both sides had is not a conflict");
    assert!(unmerged_paths(p).is_empty());
}

#[test]
fn a_conflict_resolved_in_the_file_stages_as_before() {
    // Negative: once the markers are edited away, staging is how the
    // conflict is marked resolved.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    std::fs::write(
        p.join("README.md"),
        "# Orders\n\nRun `uv run -m pricing`.\n",
    )
    .unwrap();
    stage_all(p).expect("a resolved conflict stages");
    assert!(unmerged_paths(p).is_empty());
}

#[test]
fn staging_from_a_subdirectory_checks_only_what_it_stages() {
    // The conflict sits above the subdirectory root: Stage All covers
    // the whole repository and refuses, staging a file beside it does
    // not reach the conflict and goes ahead.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    let sub = p.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("notes.txt"), "todo\n").unwrap();
    assert!(stage_all(&sub).is_err());
    stage_path(&sub, "notes.txt").expect("an unrelated file stages");
    assert_eq!(unmerged_paths(p), vec![String::from("README.md")]);
}

#[test]
fn a_repo_with_no_merge_in_progress_has_no_unmerged_paths() {
    // Negative: ordinary edits are not conflicts.
    let tmp = repo_in_a_conflicted_merge();
    let p = tmp.path();
    run_mutation(p, &["merge", "--abort"]).unwrap();
    std::fs::write(p.join("README.md"), "edited\n").unwrap();
    assert!(unmerged_paths(p).is_empty());
    commit(p, "edit").expect("an ordinary commit goes through");
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
            test_git_command()
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
            .filter(|l| l.starts_with(['-', '+']) && !l.starts_with("---") && !l.starts_with("+++"))
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
        app_git_out(tmp.path(), &["show", hash.trim(), "--", "notes.md"]),
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

/// A repository with `src/main.rs` committed on `main` and `origin` at
/// `url`; `pushed` fakes the push with a remote-tracking ref.
fn share_repo(url: &str, pushed: bool) -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    init_repo_with_commit(p);
    std::fs::create_dir(p.join("src")).unwrap();
    std::fs::write(
        p.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        assert!(
            test_git_command()
                .arg("-C")
                .arg(p)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "main"]);
    git(&["remote", "add", "origin", url]);
    if pushed {
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }
    let sha = run_git(p, &["rev-parse", "HEAD"]).unwrap();
    let file = p.join("src/main.rs");
    (tmp, file, sha)
}

/// #1610: the permalink names HEAD's commit and the selected lines, the
/// same from an ssh or an https origin; the head link names the branch.
#[test]
fn share_links_name_the_commit_or_branch_and_the_lines() {
    for url in [
        "git@github.com:acme/widgets.git",
        "https://github.com/acme/widgets",
    ] {
        let (_tmp, file, sha) = share_repo(url, true);
        assert_eq!(
            share_link(&file, Some((2, 4)), LinkRev::Commit).unwrap(),
            format!("https://github.com/acme/widgets/blob/{sha}/src/main.rs#L2-L4")
        );
        assert_eq!(
            share_link(&file, Some((2, 2)), LinkRev::Branch).unwrap(),
            "https://github.com/acme/widgets/blob/main/src/main.rs#L2"
        );
    }
    let (_tmp, file, sha) = share_repo("git@gitlab.com:acme/widgets.git", true);
    assert_eq!(
        share_link(&file, Some((1, 3)), LinkRev::Commit).unwrap(),
        format!("https://gitlab.com/acme/widgets/-/blob/{sha}/src/main.rs#L1-3")
    );
}

/// #1610 negatives: an unpushed commit or branch, an uncommitted file,
/// an unknown host and a repository with no remote each say why instead
/// of giving a link that would not open.
#[test]
fn share_links_refuse_what_the_remote_cannot_show() {
    let (tmp, file, sha) = share_repo("git@github.com:acme/widgets.git", false);
    let err = share_link(&file, None, LinkRev::Commit).unwrap_err();
    assert!(
        err.contains(&sha[..7]) && err.contains("push it first"),
        "{err}"
    );
    let err = share_link(&file, None, LinkRev::Branch).unwrap_err();
    assert!(err.contains("branch main is not on origin"), "{err}");
    std::fs::write(tmp.path().join("src/new.rs"), "x\n").unwrap();
    let _ = test_git_command()
        .arg("-C")
        .arg(tmp.path())
        .args(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .status();
    let err = share_link(&tmp.path().join("src/new.rs"), None, LinkRev::Commit).unwrap_err();
    assert!(err.contains("src/new.rs is not committed"), "{err}");
    // Staged but not committed is still not on the remote.
    let git = |args: &[&str]| {
        assert!(
            test_git_command()
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "src/new.rs"]);
    let err = share_link(&tmp.path().join("src/new.rs"), None, LinkRev::Commit).unwrap_err();
    assert!(err.contains("src/new.rs is not committed"), "{err}");
    // A local commit the tracking branch does not have yet.
    git(&["commit", "-q", "-m", "local"]);
    let err = share_link(&file, None, LinkRev::Branch).unwrap_err();
    assert!(err.contains("branch main on origin is behind"), "{err}");
    let (_t, file, _) = share_repo("git@git.example.com:acme/widgets.git", true);
    let err = share_link(&file, None, LinkRev::Commit).unwrap_err();
    assert!(
        err.contains("not a GitHub, GitLab or Bitbucket remote"),
        "{err}"
    );
    let bare = tempfile::tempdir().unwrap();
    init_repo_with_commit(bare.path());
    let err = share_link(&bare.path().join("seed.txt"), None, LinkRev::Commit).unwrap_err();
    assert_eq!(err, "the repository has no remote to link to");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("a.txt"), "x").unwrap();
    assert!(share_link(&outside.path().join("a.txt"), None, LinkRev::Commit).is_err());
}

/// #2577: a repository whose `main` has two commits, so a ref can be moved
/// back and forth between them.
fn repo_with_two_commits() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repo_with_commit(tmp.path());
    git_in(
        tmp.path(),
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    tmp
}

/// #2577: a watch on `p` with its stamps taken, and still quiet.
fn settled_watch(p: &Path) -> GitStateWatch {
    let mut watch = GitStateWatch::default();
    assert!(!watch.moved(&query(p)), "the first look only takes stamps");
    assert!(!watch.moved(&query(p)), "nothing moved yet");
    watch
}

/// #2577: a commit run outside croft moves the git state once.
#[test]
fn an_outside_commit_moves_the_git_state() {
    let tmp = repo_with_two_commits();
    let p = tmp.path();
    let mut watch = settled_watch(p);
    git_in(p, &["commit", "-q", "--allow-empty", "-m", "made outside"]);
    assert!(watch.moved(&query(p)), "the commit is seen");
    assert!(!watch.moved(&query(p)), "and only once");
}

/// #2577: staging and checkouts run outside croft move the git state.
#[test]
fn outside_staging_and_checkout_move_the_git_state() {
    let tmp = repo_with_two_commits();
    let p = tmp.path();
    std::fs::write(p.join("seed.txt"), "changed\n").unwrap();
    let mut watch = settled_watch(p);
    git_in(p, &["add", "seed.txt"]);
    assert!(watch.moved(&query(p)), "staging is seen");
    git_in(p, &["checkout", "-q", "-b", "other"]);
    assert!(watch.moved(&query(p)), "a checkout is seen");
}

/// #2577: a branch or a tag made outside croft is seen, though neither
/// moves HEAD: COMMITS labels commits with them.
#[test]
fn a_branch_or_tag_made_outside_moves_the_git_state() {
    let tmp = repo_with_two_commits();
    let p = tmp.path();
    let mut watch = settled_watch(p);
    git_in(p, &["tag", "v1"]);
    assert!(watch.moved(&query(p)), "a new tag is seen");
    git_in(p, &["branch", "other"]);
    assert!(watch.moved(&query(p)), "a new branch is seen");
}

/// #2577 and #898: croft's own status reads write nothing git's state is
/// stamped from, even when the index's stat data is out of date and a
/// plain `git status` would rewrite it, so a refresh cannot set off the
/// next one.
#[test]
fn croft_s_own_status_reads_leave_the_git_state_still() {
    let tmp = repo_with_two_commits();
    let p = tmp.path();
    let mut watch = settled_watch(p);
    // Same bytes, new mtime: the index's stat data for it is now stale.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(p.join("seed.txt"), "one\ntwo\n").unwrap();
    for _ in 0..3 {
        let status = query(p);
        let _ = query_changes(p);
        assert!(!watch.moved(&status), "a status read moved git's state");
    }
}

/// #2577: a push run outside croft moves the upstream the ahead count is
/// measured from, and that is seen.
#[test]
fn an_outside_push_moves_the_upstream() {
    let tmp = repo_with_two_commits();
    let p = tmp.path();
    // A remote that is never contacted: the upstream only has to resolve.
    git_in(
        p,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    );
    git_in(p, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    git_in(p, &["config", "branch.main.remote", "origin"]);
    git_in(p, &["config", "branch.main.merge", "refs/heads/main"]);
    let status = query(p);
    assert_eq!(status.upstream.as_deref(), Some("refs/remotes/origin/main"));
    assert_eq!(status.ahead, 1);
    let mut watch = settled_watch(p);
    // What a push leaves behind: the remote-tracking ref moved to HEAD.
    git_in(p, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert!(watch.moved(&query(p)), "the push is seen");
    assert_eq!(query(p).ahead, 0);
}

/// #2577: in a linked worktree, `.git` is a file naming the worktree's own
/// git dir, whose `commondir` names the shared one; a commit there is seen.
#[test]
fn a_commit_in_a_linked_worktree_moves_its_git_state() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("main");
    std::fs::create_dir(&p).unwrap();
    init_repo_with_commit(&p);
    let wt = tmp.path().join("wt");
    git_in(
        &p,
        &["worktree", "add", "-q", "-b", "other", wt.to_str().unwrap()],
    );
    let dirs = git_dirs(&wt).expect("the worktree's git dirs");
    let canon = |d: &Path| d.canonicalize().unwrap();
    assert_eq!(canon(&dirs.git_dir), canon(&p.join(".git/worktrees/wt")));
    assert_eq!(canon(&dirs.common_dir), canon(&p.join(".git")));
    let mut watch = settled_watch(&wt);
    git_in(
        &wt,
        &["commit", "-q", "--allow-empty", "-m", "made outside"],
    );
    assert!(watch.moved(&query(&wt)), "the worktree's commit is seen");
}
