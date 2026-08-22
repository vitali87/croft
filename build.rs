// The welcome panel used to bake `git log` output (`CROFT_RELEASE_COMMITS`)
// and the repository remote (`CROFT_REPOSITORY_REMOTE`) into the binary at
// build time. Both are gone: the panel now renders hand-curated release
// highlights from `src/release_notes.rs` (compiled-in data, zero network,
// never derived from a forge).
//
// What IS baked now is build provenance — the short git hash (`-dirty` when
// the tree has uncommitted changes) and the UTC build time — for `--version`
// and the splash badge. Two builds can share a crate version (0.1.758 was
// both the broken and the fixed binary in the 2026-08-22 session-host
// incident); provenance is what tells them apart. This is identification
// only, not forge-derived content. A tree without git (registry or git
// snapshots, tarballs) builds as `unknown`, which is itself a signal: such
// a build can never ship local changes (see `source_snapshot_warning` in
// `src/remote.rs`).
//
// Watching: cargo's default (rerun on any package-file change) misses moves
// of HEAD with no source edit — commit, branch switch, stage — which left
// the baked hash stale. So the git metadata is watched explicitly (worktree
// HEAD, index, refs, packed-refs via `--git-path`, which resolves worktree
// layouts). Emitting any rerun-if-changed disables cargo's default watch,
// so the package files that shape the binary (and flip `-dirty`) are
// re-declared alongside. Outside a git tree nothing is emitted and cargo's
// default behavior stands.
fn main() {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let suffix = if git_output(&root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
        "-dirty"
    } else {
        ""
    };
    // Two spellings of the same provenance: the short label for display
    // (`--version`, splash badge) and the full commit ID for comparison —
    // abbreviation length follows `core.abbrev`/repo size, so short labels
    // for one commit can differ between builds and would read as false
    // drift (see `probe_drift` in `src/update_watch.rs`).
    let hash = git_output(&root, &["rev-parse", "--short", "HEAD"])
        .map(|h| format!("{h}{suffix}"))
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=CROFT_GIT_HASH={hash}");
    let full = git_output(&root, &["rev-parse", "HEAD"])
        .map(|h| format!("{h}{suffix}"))
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=CROFT_GIT_HASH_FULL={full}");
    let time = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=CROFT_BUILD_TIME={time}");
    watch_provenance_inputs(&root);
}

/// Declares every input that can move `CROFT_GIT_HASH`: the git metadata the
/// hash reads, and the package files whose edits flip `-dirty`. A path is
/// only declared when it exists — a declared-but-missing path makes cargo
/// rerun on every build (`packed-refs` is the usual absentee); if it appears
/// later, the operation that creates it also rewrites `refs`, which is
/// watched.
fn watch_provenance_inputs(root: &str) {
    let Some(git_paths) = git_output(
        root,
        &[
            "rev-parse",
            "--git-path",
            "HEAD",
            "--git-path",
            "index",
            "--git-path",
            "refs",
            "--git-path",
            "packed-refs",
        ],
    ) else {
        return;
    };
    let package_files = [
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "rust-toolchain.toml",
        "src",
        "assets",
        "tests",
    ];
    for path in git_paths.lines().chain(package_files) {
        let path = std::path::Path::new(root).join(path);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn git_output(root: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
