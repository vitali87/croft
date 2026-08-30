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
// `src/remote.rs`). A checkout tracked only as a subdirectory of a larger
// repo also bakes `unknown` — telling that layout apart from a snapshot
// unpacked inside an unrelated repo isn't worth the risk of baking an
// ancestor's commit, so it deliberately trades provenance away (no warning
// covers it; the snapshot warning is path-based and won't fire).
//
// Watching: cargo's default (rerun on any package-file change) misses moves
// of HEAD with no source edit — commit, branch switch, stage — which left
// the baked hash stale. So the git metadata is watched explicitly (worktree
// HEAD, index, refs, packed-refs via `--git-path`, which resolves worktree
// layouts). Emitting any rerun-if-changed disables cargo's default watch,
// so the package files that shape the binary (and flip `-dirty`) are
// re-declared alongside. Outside a git tree nothing is emitted and cargo's
// default behavior stands.
/// Bake this version's release highlights into the binary.
///
/// One file per version (`src/release_notes/<version>.md`) rather than one
/// shared file: the CONTENT of two versions' notes never conflicts, only the
/// file did, and every open pull request had to rebase through it. Five
/// rebases in one day, plus a hand-agreed version ladder between sessions,
/// paid for this.
///
/// A missing file is a BUILD error rather than an empty panel, keeping the
/// guarantee the single file gave: a binary always describes itself.
fn bake_release_notes() {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let dir = std::path::Path::new(&root)
        .join("src")
        .join("release_notes");
    let path = dir.join(format!("{version}.md"));
    // Watch the directory: adding the next version's file must rebuild, and
    // editing this one must too.
    println!("cargo:rerun-if-changed={}", dir.display());
    let notes = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => text,
        Ok(_) => panic!(
            "{} is empty. Write this version's highlights there: one per line, \
             each prefixed `feature:` or `fix:`.",
            path.display()
        ),
        Err(e) => panic!(
            "{} could not be read ({e}). Every version needs a notes file, so \
             the welcome panel always describes the binary it is in. Create it \
             with one highlight per line, each prefixed `feature:` or `fix:`.",
            path.display()
        ),
    };
    let out =
        std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("release_notes.md");
    std::fs::write(&out, notes).expect("write release notes");
}

fn main() {
    bake_release_notes();
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    // git discovery walks UP from `-C`, so a crate unpacked inside some
    // unrelated repo (`cargo publish --dry-run` verifying under
    // `target/package/`, a registry snapshot under a $HOME that is itself a
    // git repo) would happily bake that ANCESTOR's commit as provenance.
    // Only a directory that is its own repo toplevel speaks for the source.
    if !dir_is_repo_toplevel(&root) {
        println!("cargo:rustc-env=CROFT_GIT_HASH=unknown");
        println!("cargo:rustc-env=CROFT_GIT_HASH_FULL=unknown");
        emit_build_time();
        // In this branch `.git` is absent (a dir with its own `.git` is its
        // own toplevel), so declaring it forces a rerun every build: pure git
        // operations touch no package file, and a later `git init`/adoption
        // of the tree would otherwise leave `unknown` baked until an actual
        // source edit.
        println!("cargo:rerun-if-changed={root}/.git");
        return;
    }
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
    emit_build_time();
    watch_provenance_inputs(&root);
}

/// True only when `dir` is the working-tree root of its own repository —
/// the one case where git's answers describe THIS source rather than some
/// repository that merely contains the directory.
fn dir_is_repo_toplevel(dir: &str) -> bool {
    let Some(toplevel) = git_output(dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let canon = |p: &str| std::fs::canonicalize(p).ok();
    canon(dir).is_some() && canon(dir) == canon(&toplevel)
}

fn emit_build_time() {
    let time = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=CROFT_BUILD_TIME={time}");
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
    // An inherited `GIT_DIR` (bare-dotfiles shells: `export GIT_DIR=~/.dotfiles`)
    // makes git skip discovery and treat `-C`'s dir as the worktree toplevel:
    // `--show-toplevel` then echoes the question back — satisfying the guard by
    // construction — while HEAD answers from the foreign repo. Only filesystem
    // discovery may speak for this source, so git's env overrides are dropped.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
