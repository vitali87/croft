# Contributing to croft

Thanks for hacking on croft. Build, run, and platform setup live in the
[README](README.md) and the [platform guides](docs/). Project internals are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). This guide covers the day to day
developer workflow concerns that do not belong in any of those.

Everything in this guide applies to AI agents too, including the two sections
below on coordinating work and on verifying that a review happened. Agents often
also keep a `CLAUDE.md` at the root for their own preferences, but it is
deliberately untracked and clone-local: a fresh checkout will not have one, and
nothing here depends on it.

## Coordinating work, so concurrent sessions do not collide

Several agent sessions work this repo at once. There is no lock or claim
label: an open PR against an issue is the only signal that the work is taken.
Check before starting — `gh pr list --search "<issue number>"` is one call, and
skipping it is how the same issue gets solved twice.

**A PR holds the work only while someone is behind it.** If nothing has moved,
it is not a reservation:

1. Age the work by its **last commit**, not `updatedAt` — a comment bumps
   `updatedAt`, so a stale branch can look active:
   `gh pr view <n> --json commits -q '.commits | last | .committedDate'`
2. Ask the sessions that are actually live, naming the specific PRs. One
   message is cheap; discovering ownership after merging is not.
3. Silence plus a stale commit date means it is free to take.

When you do take something over, say so on the PR with your reasons, and leave
the branch untouched and reopenable — no force-push, no rewriting someone
else's history.

## Verifying a review actually happened

A green checks column is not evidence that anyone reviewed the change. Before
merging, confirm an actual review body exists — `gh pr view <n> --json
reviews,comments` — and that every finding in it is fixed or refuted with a
reason. Two specific traps:

* A review bot's check can report **pass** while annotated "review rate
  limited", which means no review ran at all.
* `mergeStateStatus: CLEAN` answers "is a branch rule blocking this", not "has
  this been reviewed". A PR with no review at all reports CLEAN.

Re-fetch comments immediately before merging rather than trusting what you read
earlier: bot replies land asynchronously while checks are still running.

## Verifying the CHECKS actually ran

A short, all-green `gh pr checks` is not evidence that CI ran, and a green
merge gate is not evidence that anything is resolved. Every item below was hit
for real on this repo in a single day, and they share one shape: **the thing
that would have told you was absent rather than wrong.** A check that cannot
fail in the case you need it for is not a check.

**An all-green check list can mean CI never started.** As CI
gets faster this gets more dangerous, not less: a check-settled test that polls
until nothing is pending passes instantly against an empty list, because the
run has not been created yet. Count the jobs and require all of them for the
SHA you are about to merge. "Nothing is failing" and "nothing has run" are the
same reading.

**A `CONFLICTING` pull request gets no workflow runs at all.** GitHub cannot
compute the merge commit, so it never creates them. Four pushes over an hour
produced zero runs while `gh pr checks` showed one row the whole time — a
review bot's no-op — which reads exactly like a healthy PR early in its cycle.
The tell is the pair:

```bash
gh pr view <n> --json mergeable          # CONFLICTING
gh run list --branch <branch> --limit 3  # nothing for the head SHA
```

Either alone is ambiguous; together they are conclusive. This is worse than the
empty-list case above, because a single green row survives a glance that an
empty list would not.

**A job that never got a runner reports `pending`, exactly like one that is
running.** `gh run view <id> --json jobs` is the right command, but read
`status`, not `completedAt`. That field is Go's zero `time.Time`
(`0001-01-01T00:00:00Z`) for every job that has not finished, `in_progress` and
`queued` alike, so it separates finished from unfinished — which is not the
question. `startedAt` is populated on queued jobs too. What distinguishes them
is a job still `queued` while its siblings in the same run have moved to
`in_progress` or `completed`.

**Threads can appear after everything is green, and an earlier read of them
expires.** The rule above about a bot whose check says pass while no review ran
has a second direction, and it nearly landed a major on #392: a bot that has
been genuinely silent can start producing findings at any moment, and its check
row looks identical before and after. That PR was eight-of-eight green — where
one of the eight was the review bot's own `pass`, annotated "Review rate
limited" — when a pre-merge re-fetch turned up five inline threads, one of them
a command running in a directory other than the one its confirm popup named.

Read them with the GraphQL `reviewThreads` query rather than
`pulls/<n>/comments`: the REST payload carries no resolution state at all, and
resolution is what the ruleset below gates on.

```bash
gh api graphql -f query='
{ repository(owner:"OWNER", name:"REPO") { pullRequest(number:N) {
  reviewThreads(first:50) { nodes { isResolved isOutdated path } } } } }'
```

`isOutdated` earns its place beside `isResolved`: a thread on a file your branch
no longer owns appears there, and no code change will ever resolve it.

**`mergeStateStatus: BLOCKED` names no reason, and the obvious endpoint lies.**
Classic branch protection can report zero required checks and zero required
approvals while a **ruleset** is what is actually enforcing. Rulesets live
somewhere else entirely:

```bash
gh api repos/<repo>/rules/branches/main
```

On this repo that is where `required_review_thread_resolution` lives, which is
why a PR with every check green and every finding fixed in code still refuses
to merge until the threads themselves are resolved. Fixing the code does not
resolve a thread, and a thread on a file your branch no longer owns cannot be
resolved by any code change at all — reply saying where the point was addressed,
then resolve it.

**Your version was valid when you branched and is stale by the time you merge.**
The release gate compares your head against the merge base, so it passes as
long as your branch is above main *at that point*. Two branches can both pass
legitimately and only the second to merge conflicts. Re-read
`git show origin/main:Cargo.toml` in the same breath as the final
`gh pr checks`, which is one command.

This rule is repeated in `CLAUDE.md`, and this copy is the one to trust:
`/CLAUDE.md` is gitignored, so a fresh clone never sees it.

## Every shipped change is a release

A PR that changes anything compiled into the binary (`src/`, `assets/`,
`build.rs`, `Cargo.toml`, `Cargo.lock`) must also:

* **bump `version` in `Cargo.toml`** — two different binaries must never share
  a version and differ only in commit hash, and
* **write `src/release_notes/<version>.md`** — one file per version, named
  for the version in `Cargo.toml`. The welcome panel's "IN THIS RELEASE" card
  describes the single version it is baked into, so a missing or stale list
  means the panel lies about what the running build ships.

One highlight per line, each prefixed `feature:` or `fix:`, which selects the
card's glyph and tint. Blank lines and `#` headings are ignored:

```text
feature: Cmd+F now searches a rendered colour log.
fix: A copy larger than the cap no longer splits a character.
```

A missing file for the current version is a **build** error, not an empty
panel, so a binary always describes itself.

Notes live in one file per version rather than one shared file because two
versions' notes never conflict in content, only in the file they shared: with
several PRs open, every merge forced a rebase through it, and the version
number had to be reserved by hand between contributors (#399). Only the
current version's file may change in a PR: an older one describes a release
that has already shipped.

CI enforces all of it (the `version bump + release notes` job). Docs, CI, and
test-only PRs (`src/app/tests.rs`, `tests/`) are exempt.

## Insert new items AFTER a complete item, never above a doc block

Rust attaches a `///` block to whatever item **follows** it. Insert anything
between an existing item and its doc comment and that prose silently becomes
the newcomer's: no compiler error, no failing test, no clippy lint. The build
stays green and the rendered rustdoc is *confidently wrong* rather than
absent, which is worse - absent docs send a reader to the code, wrong docs
stop them looking.

The habit that avoids it entirely is positional: add a new item after a
complete item, not directly above a `///` block. Where that is not possible,
confirm the doc block above the **next** item still describes that next item.

This is not only about functions. A `const` inserted above another `const`'s
doc captures it exactly the same way, and did so twice in one day before the
gate could see it.

CI catches what the habit misses (the `doc comments stay with their function`
job): an item that had a doc comment at the merge base and has none at your
head is the fingerprint this insertion leaves. It covers `fn`, `const`,
`static`, `struct`, `enum`, `union`, `trait`, `type` and `macro_rules!`, and
for a file your branch ADDS it compares your commits pairwise, since a file
with no base version has no merge-base history to lose documentation against. If a removal is deliberate, say
so in a commit message on the branch:

```text
doc-removal: src/path/to/file.rs::some_function_name
doc-removal: src/path/to/file.rs::SomeType::method_name
```

The key after the path is the one the gate's own error names: a bare name
for a free item, or the enclosing `impl` header for a method (`Foo::new`,
`Display for Foo::fmt`), so a declared removal of one `new` cannot excuse
another.

The file qualifier matters: an exemption keyed on the bare name would excuse
every function of that name in every changed file, so a deliberate removal of
one `new` would quietly cover an accidental loss of another.

Run it yourself with `python3 scripts/check_doc_ownership.py origin/main HEAD`.

## Managing the `target/` directory

croft is a large workspace with a deep dependency tree, and active development
means frequent rebuilds. Cargo optimises for build speed by keeping the
incremental compilation cache (`target/debug/incremental/`) plus a compiled copy
of every crate in the tree. The catch is that Cargo **does not garbage collect
the per project `target/` directory**: old incremental snapshots from previous
branches and toolchains accumulate and are never reclaimed. On a busy croft
checkout this directory can grow into the hundreds of gigabytes, the bulk of it
stale incremental cache.

Two things worth knowing:

* Cargo's built in automatic cache cleanup (stable since 1.88) only prunes the
  **global** cache under `~/.cargo` (downloaded registry and git sources). It
  never touches a project's `target/`, so it does nothing for the directory that
  actually grows.
* A one off `cargo clean` wipes `target/` entirely, which reclaims everything
  but forces a full cold rebuild next time. Fine in an emergency, painful as a
  routine.

### The recommended fix: scheduled `cargo-sweep`

[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) deletes only the build
artifacts that have not been used for N days. Your active branch stays warm and
rebuilds fast while stale snapshots get reclaimed. Run it on a timer and you
never have to think about disk space again.

Install it:

```bash
cargo install cargo-sweep
```

Run it by hand whenever you want, recursively across all your Rust projects:

```bash
# Preview first (no deletions)
cargo sweep -r --dry-run --time 15 ~/path/to/projects

# Reclaim artifacts unused for 15+ days
cargo sweep -r --time 15 ~/path/to/projects
```

`--time 15` keeps anything touched in the last 15 days. Lower it if you switch
branches a lot and the cache still grows faster than you would like.

### Automate it (recommended)

Run the sweep weekly so it stays hands off.

**macOS (launchd).** Save as
`~/Library/LaunchAgents/com.user.cargo-sweep.plist`, adjusting the path to where
you keep your projects:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.user.cargo-sweep</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/zsh</string>
        <string>-lc</string>
        <string>cargo sweep -r --time 15 "$HOME/path/to/projects"</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Weekday</key><integer>0</integer>
        <key>Hour</key><integer>3</integer>
        <key>Minute</key><integer>0</integer>
    </dict>
    <key>StandardOutPath</key>
    <string>/Users/YOU/Library/Logs/cargo-sweep.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/YOU/Library/Logs/cargo-sweep.log</string>
</dict>
</plist>
```

Then load it:

```bash
launchctl load ~/Library/LaunchAgents/com.user.cargo-sweep.plist
launchctl list | grep cargo-sweep   # confirm it registered
```

Invoking through `zsh -lc` matters: launchd runs with a bare environment, and a
login shell puts `cargo` (via rustup) on `PATH`. Missed runs (machine asleep at
03:00) fire on the next wake.

**Linux (cron).** Add a weekly entry with `crontab -e`:

```cron
0 3 * * 0 $HOME/.cargo/bin/cargo-sweep -r --time 15 "$HOME/path/to/projects" >> "$HOME/.cache/cargo-sweep.log" 2>&1
```

Or, if you prefer systemd, a user `cargo-sweep.timer` paired with a
`cargo-sweep.service` running the same command achieves the same thing.

### Why not turn off incremental compilation?

Setting `incremental = false` would stop the cache from growing, but it slows
the edit, build, run loop that you rely on while developing croft. Keep
incremental on and let `cargo-sweep` reclaim the stale parts instead.

### macOS: keep the build directory out of Spotlight

Disk is not the only cost of the build directory on macOS. Spotlight indexes the
constant `.fingerprint/` and `incremental/` churn, which pins a CPU core and
spins your fans up while you build. The fix is to build into a directory whose
name ends in `.noindex`, which Spotlight ignores. See
[docs/MACOS.md](docs/MACOS.md#spotlight-indexing-and-the-build-directory) for the
one line setup. With it applied your build directory is `target.noindex/` rather
than `target/`.

## Running the suite: cap your thread count

The suite spawns PTYs and real shells, so a slice of it is timing-sensitive and
starves under contention. Run it flat out on a many-core machine and you get
failures that have nothing to do with your change — terminal, clipboard and
pairing tests that pass fine on an idle box. CI pins `RUST_TEST_THREADS: 4` for
exactly this reason.

Half your cores is a reasonable default:

```bash
RUST_TEST_THREADS=$(( ($(getconf _NPROCESSORS_ONLN) + 1) / 2 )) cargo test
```

To make it permanent, the right number is per-machine, so both files are
gitignored rather than committed: `/.cargo/config.toml` caps build jobs
(`[build] jobs = N`), and `/.config/nextest.toml` caps nextest's threads —
nextest reads them from there, not from `RUST_TEST_THREADS`.

Before you blame your change for a terminal or clipboard failure, re-run it
against an untouched `origin/main` checkout. These flake under load, and
baselining is faster than bisecting.

## Waiting on a spawned process in a test

A test that spawns a real process and waits a **fixed** wall-clock budget will
flake on a loaded machine, and the budget looks generous right up until it
isn't. The number is not knowable from inside the test: what blows it is not
the operation, it is contention from every other test spawning at the same
moment, plus whatever else owns the machine.

So do not pick a fresh constant. Use the shared helper, which scales a quiet
machine baseline by the load actually present:

```rust
crate::test_budget::await_spawned(
    Duration::from_millis(500),          // what it costs on a quiet machine
    "the shell to paint the linked cell", // what you are waiting for
    || linked_cell(&app).is_some(),
);
```

For a wait that hands its deadline to something else (a `recv_timeout`, a
probe's own timeout) use `test_budget::spawn_budget(base)` for the `Duration`.

The teeth are unchanged: a genuinely broken behaviour never satisfies the
condition and still fails, just later. That trade - a slow true failure over a
fast false one - is the point.

**When one of these does fail, the cheap first move is the merge-base
comparison:** run the full suite on the unmodified merge base under the same
load. If it fails there too, your diff is innocent. Isolation runs cannot tell
you this, because an isolated run cannot reproduce a contention failure however
many times you repeat it.

## Bumping the Rust toolchain

`rust-toolchain.toml` is the single source of truth for the channel, and
**rustup targets belong to one toolchain**. Bumping the pin orphans every
cross target, which silently turns `croft <host>` from "ship a prebuilt static
binary" into "compile the whole crate graph on the user's box". A 1.95.0 to
1.97.1 bump did exactly that for four days.

So a toolchain bump is not finished until this passes, run **from inside the
checkout** so the pin applies:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo zigbuild --profile remote-fast --locked --bin croft \
  --target x86_64-unknown-linux-musl
```

The binary has to exist at the end. `remote::tests::the_pinned_toolchain_has_every_cross_target`
fails the suite when a target is missing, and `.github/workflows/ci.yml` runs
the real ship-path build for both musl triples plus an Android NDK build on
every pull request.

When a remote update feels slow, read `~/.cache/croft/install.log` first: it
records the exact reason the fast path was skipped.
