# Contributing to croft

Thanks for hacking on croft. Build, run, and platform setup live in the
[README](README.md) and the [platform guides](docs/). Project internals are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). This guide covers the day to day
developer workflow concerns that do not belong in any of those.

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
