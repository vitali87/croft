# Contributing to croft

Thanks for helping. This page covers what you need to land a change. For
building and running croft see the [README](README.md); for how the code is laid
out see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). AI agents follow this page
too.

Every change is weighed against the [tenets](README.md#tenets): speed and low
latency first, the same behaviour locally and over SSH, a shortcut for every
action, and bugs fixed at the root rather than worked around.

## Set up

```bash
git clone https://github.com/vitali87/croft.git && cd croft
cargo run -- .        # rustup installs the pinned toolchain (rust-toolchain.toml)
```

The full test suite also needs `zsh` at `/bin/zsh` and `pdftoppm` (poppler-utils)
on `PATH`, plus `python3` for the scripts in `scripts/`. On macOS, set up a
`.noindex` build directory first, or Spotlight will index it and spin your fans:
see [docs/MACOS.md](docs/MACOS.md#spotlight-indexing-and-the-build-directory).

## Pick something to work on

1. Start from an issue. Issues labelled `ready`, or opened by the maintainer, are
   vetted and fair game. For anything new, open an issue first.
2. Check that no open PR already references it. There is no claim label: an open
   PR is the claim.

## Make the change

* **Keep PRs small.** The PR Split Score check flags anything over about 400
  changed lines. Split bigger work into stacked PRs.
* **Tests** go beside the code in a `#[cfg(test)]` module. Whole-app behaviour is
  tested in `src/app/tests.rs`, and the CLI in `tests/`.
* **Commits** are one imperative sentence saying what changed for the user, for
  example `Keep folds collapsed when lines are added or deleted`. No prefixes.
* **Keep your branch current by merging `main` into it.** Don't rebase or
  force-push a branch someone else is working on.
* **Insert new items after a complete item, never just above a `///` block.**
  Rust attaches a doc comment to whatever follows it, so inserting there gives
  the old item's docs to the new one, and nothing fails. The
  `doc comments stay with their function` CI job catches it. If you are removing
  a doc on purpose, add `doc-removal: src/path/to/file.rs::name` to a commit
  message. The CI error tells you the exact key to use.
* **New or changed module?** Update its entry in
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). CI fails if you shorten a
  module's description without moving its reasoning into a `### <module>`
  section.

## Every shipped change carries release notes

If your PR changes `src/`, `assets/`, `build.rs`, `Cargo.toml` or `Cargo.lock`,
add one file named `src/release_notes/unreleased/<issue>-<slug>.md`, for example
`862-hot-exit.md`. Put one user-facing highlight on each line:

```text
feature: Unsaved edits survive a kill and come back on the next launch.
fix: Ctrl+Q asks before throwing away unsaved edits.
```

**Never** change `version` in `Cargo.toml`, and never edit
`src/release_notes/<version>.md`. After your PR merges, CI assigns the version,
folds in the notes, and publishes the release. You don't need a fragment for
docs, CI or test-only changes. A diff confined to `#[cfg(test)]`,
`src/app/tests.rs` or `tests/` counts as test-only.

## Show it

If a user can see the change (a view, modal, command, status message,
keybinding, layout or colour), the PR description needs a recording of it. Use a
**GIF** for interaction and a **screenshot** for a still. Record the real binary
built from your branch. Show the feature working and the edge case a reviewer
will ask about, such as an error or an empty state. Aim for 30 seconds or less
and under 1 MB. If nothing visible changed, replace the template's Demo section
with one line saying why.

To record, use vhs, asciinema with agg, or any screen recorder, then drag the
file into the PR description.

<details>
<summary>Recording without a browser (agents, headless machines)</summary>

`scripts/demo/tui_demo.py` drives the real binary in tmux and writes a GIF. It
needs tmux, node, Pillow and Chromium. Copy the scenario in
`scripts/demo/examples/`:

```bash
cargo build
python3 scripts/demo/examples/settings_editor.py target/debug/croft out.gif
```

If you have push access, commit the GIF to the never-merged `pr-media` branch at
`<pr>/<name>.gif`. Then embed it with a URL pinned to that commit's SHA, so the
image stays the same after later pushes:

```bash
git fetch origin pr-media && git worktree add ../pr-media origin/pr-media
mkdir -p ../pr-media/<pr> && cp out.gif ../pr-media/<pr>/<name>.gif
git -C ../pr-media add -A && git -C ../pr-media commit -m "pr-media: <what> (#<pr>)"
git -C ../pr-media push origin HEAD:pr-media && git -C ../pr-media rev-parse HEAD
```

```markdown
![what it shows](https://raw.githubusercontent.com/vitali87/croft/<sha>/<pr>/<name>.gif)
```

</details>

## Check before you push

CI runs all of these. Running them locally first saves a round trip:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
RUST_TEST_THREADS=4 cargo test --locked
python3 -m unittest discover -s scripts/tests

base=$(git merge-base origin/main HEAD)
python3 scripts/release.py check "$base" HEAD           # release notes
python3 scripts/check_doc_ownership.py "$base" HEAD     # doc comments
```

CI also cross-builds the static musl binaries that `croft remote` ships, and an
Android build. If you bump `rust-toolchain.toml`, add the cross targets
(`rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`) and
make sure those jobs stay green.

### Flaky tests

The suite spawns real PTYs and shells, so terminal, clipboard and pairing tests
can fail on a busy machine. Cap the thread count at about half your cores, as
above. To set it permanently, use the gitignored `/.cargo/config.toml` (build
jobs) and `/.config/nextest.toml` (nextest threads). Before you blame your
change, run the same tests on an untouched `main` under the same load. If they
fail there too, your diff is not the cause.

One wall-clock test is `#[ignore]`d and runs on its own:
`cargo test --bin croft fs_sync_reflects -- --ignored --test-threads=1`.

### Waiting on a spawned process in a test

A fixed timeout will eventually flake on a loaded machine. Use the shared helper,
which scales a quiet-machine baseline by the current load:

```rust
crate::test_budget::await_spawned(
    Duration::from_millis(500),           // cost on a quiet machine
    "the shell to paint the linked cell", // what you are waiting for
    || linked_cell(&app).is_some(),
);
```

To get a `Duration` you can pass on, such as to `recv_timeout`, use
`test_budget::spawn_budget(base)`.

## Review and merge

Bots (CodeRabbit, Greptile and Copilot) review every PR. They are tuned to flag
only bugs, security issues and data loss. Fix each thread or reply saying why you didn't, then **resolve** it. Merge
is blocked until every thread is resolved. PRs are merged with a merge commit.

## Keep `target/` from filling your disk

Cargo never cleans a project's `target/`, so it can grow to hundreds of GB.
[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) deletes only artifacts
you haven't used recently. Run it weekly from cron or launchd:

```bash
cargo install cargo-sweep
cargo sweep -r --time 15 ~/path/to/projects   # drop artifacts unused for 15+ days
```
