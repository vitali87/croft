# Working on croft as an agent

Notes for AI agents (and anyone else) working this repo. Contributor workflow —
the release gate, `target/` hygiene, test layout — lives in
[CONTRIBUTING.md](CONTRIBUTING.md); this file covers what is specific to
several agents working the same repo at once.

## Claim work before you start: the `claimed` label

Several sessions often work this repo in parallel, in separate clones and
worktrees, with no shared view of each other. Two of them picking up the same
PR means duplicated review rounds, conflicting pushes, and — in the worst case
— one session force-pushing over another's work.

**Before starting on a PR or an issue, check for the `claimed` label. If it is
there, someone is already on it: leave it alone.** If you think you need it
anyway, ask the holder first and the user if you cannot reach them.

**When you start work, add the label. When you stop, remove it.**

```bash
gh pr view <n> --json labels --jq '[.labels[].name]'   # check first
gh pr edit <n> --add-label claimed                     # claiming
gh pr edit <n> --remove-label claimed                  # done, or handing off
```

Use `gh issue view` / `gh issue edit` for issues. To see everything currently
claimed: `gh pr list --label claimed` and `gh issue list --label claimed`.

Where to put it: **claim the PR once one exists**, since that is where the
contended work happens — pushes, review rounds, the merge. Claim the issue only
for work that has no PR yet, and move the claim to the PR when you open it.

### Releasing it

The label is a lock, and a lock nobody releases is worse than no lock. Remove it
when you merge, when you stop working, and when you hand off. Merging usually
deletes the branch but **not** the label, so drop it as part of the merge.

The two failure modes are not symmetric, which is why releasing belongs in the
merge rather than in a follow-up step: forgetting to *claim* costs you one
possible collision, while forgetting to *release* blocks the work indefinitely,
and nothing about an abandoned claim distinguishes it from an active one.

It deliberately records no owner, so it cannot tell you *who* holds a claim or
*when* they took it. That keeps it cheap, at the cost of being unable to
distinguish an active claim from an abandoned one. If a claim looks stale, do
not just assume it — the sessions working this repo can talk to each other, so
ask the holder first, and ask the user if you cannot reach them.

## Coordinating with other sessions

Claiming is the cheap signal, not a substitute for talking. Peer sessions on the
same machine are discoverable and can be messaged directly, which resolves
ownership questions far more reliably than guessing from timestamps or commit
authorship — every session commits under the same user identity, so neither one
tells you which session did the work.

Two habits that avoid the common collisions:

* **Re-check state immediately before you act on it.** Both the version bump and
  the pre-merge review sweep have been broken by state moving during the window
  between deciding and merging. Re-read rather than reusing a value you fetched
  minutes ago.

  For the version specifically, note that the release gate **cannot** catch a
  collision itself — it is answering a different question. It compares your head
  against the merge base, so it passes as long as your branch is above main *at
  that point*, which means two branches can both pass legitimately and only the
  second one to merge conflicts. Running the gate later would not help. Re-read
  `git show origin/main:Cargo.toml` in the same breath as the final
  `gh pr checks`, and bump again if main has moved.
* **Never rewrite a branch you do not hold.** No force-pushes to another
  session's PR. If a branch needs main, merge main into it — that is the
  convention here, and it keeps the push a fast-forward.

## Verifying a review actually happened

Before merging on a bot review, confirm the review ARTIFACT exists rather than
trusting an aggregate signal. CodeRabbit's check on this repo has reported
`pass` while annotated "Review rate limited" — a green row meaning no review
happened at all. Zero unresolved threads reads the same whether the bot found
nothing or never ran, and a bot that edits its summary comment in place makes
any earlier read stale.

Query both `.comments[]` and `.reviews[]`: they carry different artifacts, not
duplicates, and an entry can be present with an empty body — a container for
inline threads rather than a verdict. Require the body to be non-empty *and* to
carry the verdict you are looking for; presence alone is not evidence. Exclude
the PR author, whose own inline replies land there as empty entries too,
otherwise a PR looks more reviewed the more diligently its author answers
feedback. And do not assume the shape is stable: the same bots on the same repo
produce an empty `reviews[]` on one PR and dozens of entries on another, so
"here the verdict lives in field X" is never a safe shortcut.

Anchor it to the head SHA. If the artifact names a commit other than the head
you are about to merge, the review is stale and does not count, however good it
looks. A stale review and a missing review have the same consequence: do not
merge yet. "No review yet" and "reviewed and clean" must never collapse into the
same verdict.

Everything above is a habit, and a hurried session can skip a habit. The sibling
code-graph-rag repo enforces the equivalent mechanically instead: a required
`Greptile 5/5 Gate` job polls until a scored review names the exact head SHA,
and fails on a stale, missing, or lower-scored one, so the merge is blocked
rather than merely discouraged. croft has no such gate today. If these rules
keep costing attention, porting it is the fix worth making.
