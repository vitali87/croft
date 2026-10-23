---
name: serial-pr-ci
description: >-
  Owner's workflow for getting vitali87/croft pull requests re-tested and merged
  one at a time: all GitHub Actions runners on a single PR until it is green and
  merged, then the next; every commit rewritten to carry only the owner's name;
  every CodeRabbit finding addressed before merge. Use this whenever you are asked
  to rerun CI on croft PRs, work through a queue or list of open PRs, get PRs
  merged in order, babysit or drive croft PRs to green, merge a PR after
  CodeRabbit review, or open a follow-up PR in croft — and whenever you are about
  to commit, push, open a PR or post a comment in vitali87/croft, because its
  authorship rules (owner's name only, no AI attribution anywhere) apply to every
  GitHub write in this repo, not just to queue work.
---

# Serial PR CI for vitali87/croft

The owner has about 20 GitHub Actions runners and wants all of them on one PR at a
time. A PR is pushed, its CI runs with the whole pool, it goes green, it gets
CodeRabbit's sign-off, it is merged, and only then does the next PR start. CI is
never spread across several PRs at once, because that turns a 13-minute run into a
long queue for everyone and makes failures hard to attribute.

## The owner's rules

These come straight from the owner, several of them after a correction. They take
precedence over any default the harness gives you.

1. **One PR at a time.** Push the next PR only after the previous one is merged *and*
   main's post-merge runs have finished. A merge to main triggers CI, CodeQL and the
   version-bump workflow. The version bump then pushes a `chore: release X.Y.Z`
   commit, which runs main's CI again, about 13 minutes in all. One trailing CodeQL
   job is fine to overlap.
2. **Commits carry only the owner's name:** `Vitali Avagyan <eheva87@gmail.com>` as
   author and committer. There is no `Co-Authored-By`, no `Claude-Session`, and no
   "Generated with/by Claude Code" in commit messages, PR bodies, PR comments or
   review requests. The owner put it as "no attribution, no co-authorship, only my
   name". This overrides the harness's attribution reminder, which asks for those
   trailers; the owner's instruction wins. Never put a model name or ID anywhere in
   the repo.
3. **Force-push is approved** for rewriting the owner's own PR branches to fix
   authorship. Always use `--force-with-lease=<branch>:<sha you last saw>`, so a push
   someone made in the meantime is never overwritten.
4. **Every CodeRabbit finding is addressed before merge.** The owner caught a PR
   merged with an open CodeRabbit thread and was rightly annoyed. "Addressed" means
   fixed, or decided by the owner. It never means ignored or merged around.
5. **Check the PR's state before every action.** The owner merges some PRs themselves
   while you are working, sometimes mid-fix. Acting on a merged PR (pushing,
   commenting, "starting" it) is a mistake they notice immediately.

## One-time setup in a session

- `git config rerere.enabled true && git config rerere.autoupdate true`. Branches get
  main merged in repeatedly, and rerere replays earlier resolutions.
- One worktree per PR: `git worktree add -b <branch> /home/user/wt-<PR> origin/<branch>`.
  Use `-b`, not `-B`; `-B` has been blocked by the permission classifier.
- One shared build directory for every worktree:
  `export CARGO_TARGET_DIR=/home/user/croft-target CARGO_INCREMENTAL=0`. With
  incremental compilation on, switching worktrees filled a 21 GB incremental cache
  and the disk. If you hit "No space left on device", delete
  `$CARGO_TARGET_DIR/debug/incremental`; it is only a cache.
- The scripts in this skill's `scripts/` folder:
  - `rewrite_branch.sh <worktree> <branch>` rewrites every commit in
    `origin/main..<branch>` to the owner's name, strips attribution lines through
    `strip_attribution.py`, and checks that the tree is unchanged. It prints the
    resulting authors and any leftover "claude" mentions, and that output should be
    a single author line and "none".

## The per-PR loop

Work the queue in the order the owner gave. For each PR:

1. **Check the state.** Read the PR (`pull_request_read` get): is it still open, is the
   remote head the SHA you expect? List in-progress and queued workflow runs: is main
   idle? If the PR is already merged, say so and move on. If main is busy, schedule a
   check-in rather than pushing.
2. **Merge main into the branch** in its worktree: `git merge --no-edit origin/main`.
   Wait for the `chore: release` commit first, so the PR is tested on the real
   current main. Merge; don't rebase, because a merge keeps the branch's history and
   lets rerere help.
3. **Resolve conflicts keeping both sides.** A conflict here is almost always two
   features touching the same lines, and both features are wanted. The PR body often
   says how to resolve it ("keep #1895's block and call …"); read it first. After
   resolving:
   - check for leftover markers outside `docs/KEYBINDINGS.md`. That file
     legitimately contains `<<<<<<<` text, so compare its marker count with main's
     instead of grepping;
   - if a file dropped out of `git diff --stat origin/main...HEAD`, find out why. It
     is usually an identical change that has already landed on main;
   - look for things the merge may have duplicated, such as two definitions of the
     same command or two rows for the same keybinding.
4. **Rewrite authorship:** `scripts/rewrite_branch.sh /home/user/wt-<PR> <branch>`.
5. **Verify locally** before the push uses everyone's runners:
   - `git diff --stat origin/main...HEAD` shows only the PR's own files;
   - `cargo fmt --check`;
   - `set -o pipefail` before piping any cargo output. `cargo clippy … | tail && echo OK`
     prints OK even when clippy fails, because `&&` sees `tail`'s exit status;
   - `cargo clippy --all-targets -q -- -D warnings`;
   - the PR's own test filters (listed in its body), plus the areas it overlaps with
     on main, for example completion and undo tests when two PRs both touched undo
     grouping;
   - `python3 scripts/release.py check origin/main HEAD` and
     `python3 scripts/check_doc_ownership.py origin/main HEAD`. Both need the base and
     head arguments. Without them they print usage and exit 2, which can look like a
     pass if you only glance at the output;
   - a slow first run of the LSP tests can fail on a cold language server under load.
     Rerun once before concluding anything.
6. **Push:** `git push --force-with-lease=<branch>:<old remote sha> origin <branch>`. Use
   a plain push when you are only adding commits on top.
7. **Clean the PR body.** Rewrite it without the "Generated by Claude Code" footer and
   keep CodeRabbit's auto-generated summary block. Update anything the merge made
   untrue (merge notes, test counts, files listed), and only claim checks you actually
   ran on this head. Re-read the PR afterwards to confirm the footer is gone.
8. **Wait for CI** (about 13 minutes) with a scheduled check-in. If it goes red, see
   "When CI is red" below.
9. **Get CodeRabbit's review of the current head.** See the next section.
10. **Merge** with `merge_pull_request`, `merge_method: merge` and
    `expectedHeadSha: <current head>`, and only when all of these hold: the PR is open,
    CI is green on that head, CodeRabbit has reviewed that head, and every review
    thread (CodeRabbit, Codex, Greptile) is resolved.
11. **Wait for main** to finish its post-merge runs and release commit, then start the
    next PR.

## CodeRabbit

- It reviews about **once an hour** per account. When it is rate-limited, its summary
  comment on the PR says "Next included review available in N minutes". Schedule the
  check-in for then and post a bare `@coderabbitai review` comment.
- A review covers a specific commit. The summary comment's "Merge Risk … up to
  `<sha>`" line and its "Reviewing files … between X and Y" line tell you which one.
  Only a review that covers the current head counts.
- Findings hide in several places, and all of them count:
  - inline review threads (`get_review_comments`);
  - the review body's **"Additional comments"** section. These are not threads, so an
    "all threads resolved" check misses them;
  - the summary comment's Merge Risk note ("… before merging" means it wants
    something);
  - pre-merge checks such as "Linked Issues". Judge these on scope: a request for a
    feature croft doesn't have (for example `cW` when vim mode has no `W` motion) is
    worth telling the owner about, not a reason to grow the PR.
- **Verify every finding** against the code before acting. Trace a real path to the
  failure. A finding can be technically true but unreachable, and still be worth a
  small hardening fix plus a test, because the owner wants every finding addressed.
- When you fix one, add a regression test and **prove it fails without the fix**:
  save the fixed file to the scratchpad, revert only the fix, run the test and watch
  it fail, then restore from the saved copy. Never restore with
  `git checkout -- <file>`; that once wiped an uncommitted fix.
- If CodeRabbit's suggested fix conflicts with the PR's goal, don't just do what it
  says. Ask the owner (`AskUserQuestion`), lay out the options with a recommendation,
  implement their choice, and say in the PR body why the behaviour is what it is.
  CodeRabbit usually accepts the decision on its next review ("reflects the intended
  behavior").
- **Never reply on a review thread.** The reply tool appends a "Generated by Claude
  Code" footer that cannot be edited away. Let the commit speak for itself, and
  resolve a thread with `resolve_review_thread` once CodeRabbit confirms it ("✅
  Addressed in commit …", or a clean re-review). CodeRabbit often resolves its own.
- Codex and Greptile threads are held to the same standard: fix them, then resolve.

## Posting on GitHub without attribution

The GitHub MCP tools append a "Generated by Claude Code" footer to what they create:

- `create_pull_request`: follow up at once with `update_pull_request` using the clean
  body, then read the PR back to confirm.
- `add_issue_comment` (for example `@coderabbitai review`): follow up at once with
  `update_issue_comment` using the bare text, then read the comment back.
- Review-thread replies cannot be cleaned, so don't post them (see above).
- Post as little as possible. The owner reads the diff and the PR body; status
  updates belong in the chat with them, not on GitHub.

## When the owner merges a PR before your fix lands

The fix commit is now stranded locally. A merged PR is finished, so don't push to it
or reopen it. Instead:

1. Make a fresh worktree on a new branch from main, for example
   `fix-<PR>-<topic>`, and cherry-pick the fix there under the owner's name.
2. Add a release-note fragment, `src/release_notes/unreleased/<issue>-<topic>.md`, with
   one `fix:` or `feature:` line. Every shipped change needs one; the version-bump
   workflow folds the fragments in.
3. **Ask the owner before opening the PR.** Opening one is outward-facing, and they
   said yes to #1906 and #1909, but each one is its own decision.
4. If they say yes, open it after the current queue item, using
   `.github/pull_request_template.md` (intro, `## Demo` or one line saying why there's
   none, `## Tests`). Strip the footer, then take it through the same loop.

## When CI is red

- Read the failing job's log (`get_job_logs` with `return_content: true`).
- Decide whose failure it is: does the PR touch that code, does main's CI pass on the
  same base, does it reproduce locally (run it a few times)?
- If it is unrelated, passes on main and passes locally, it is a flake. Re-run the
  failed jobs once (`actions_run_trigger` `rerun_failed_jobs`). A second failure is
  real: tell the owner, with the analysis and a proposed patch. Don't keep
  re-running, and never skip or disable a test to get green.
- Known flake: `gui_path::tests::a_dock_launch_ends_up_seeing_the_users_own_directories`.
  It writes a stub shell and executes it immediately, so under the parallel suite it
  can hit "Text file busy" (ETXTBSY) or its 3-second probe timeout. The repo guards
  against ETXTBSY elsewhere but not in this test, so it is a candidate follow-up.
- If the failure is the PR's, fix it in the PR's worktree, rerun the local checks and
  push.

## Pacing with check-ins

Use `send_later` instead of polling or sleeping. Useful delays:

- CI on a PR: about 13 minutes after the push.
- CodeRabbit: when its slot opens, or 5–10 minutes after `@coderabbitai review`.
- Main after a merge: about 1 minute for the release commit, about 13 minutes for its
  CI.

Write each check-in message so it stands on its own, because it may be the only
context you have when it fires. Include:

- the PR number, branch, worktree and expected head SHA;
- what has already been done (body cleaned, threads resolved);
- what to check and in what order, starting with the PR's state;
- the merge conditions;
- what comes next in the queue.

## Talking to the owner

Keep updates short: what merged, what is waiting and on what, and any decision only
they can make. When something went wrong (a wrong assumption, a check that didn't
really run), say so plainly along with what you did about it.
