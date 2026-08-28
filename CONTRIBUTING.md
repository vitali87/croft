# Contributing to croft

Thanks for hacking on croft. Build, run, and platform setup live in the
[README](README.md) and the [platform guides](docs/). Project internals are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). This guide covers the day to day
developer workflow concerns that do not belong in any of those.

If you are an AI agent, read [CLAUDE.md](CLAUDE.md) as well: it covers claiming
work so concurrent sessions do not collide, and verifying that a review actually
happened before merging. Everything in this guide applies to you too.

## Every shipped change is a release

A PR that changes anything compiled into the binary (`src/`, `assets/`,
`build.rs`, `Cargo.toml`, `Cargo.lock`) must also:

* **bump `version` in `Cargo.toml`** — two different binaries must never share
  a version and differ only in commit hash, and
* **replace the highlights in `src/release_notes.rs`** — the welcome panel's
  "IN THIS RELEASE" card describes the single version it is baked into, so a
  stale list means the panel lies about what the running build ships.

CI enforces both (the `version bump + release notes` job). Docs, CI, and
test-only PRs (`src/app/tests.rs`, `tests/`) are exempt.

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
