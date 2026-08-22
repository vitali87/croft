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
// No `cargo:rerun-if-changed` on purpose: cargo's default then reruns this
// script whenever any package file changes, which keeps the `-dirty` flag
// honest. The two git invocations cost milliseconds.
fn main() {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let hash = git_output(&root, &["rev-parse", "--short", "HEAD"])
        .map(|h| {
            let dirty =
                git_output(&root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{h}-dirty") } else { h }
        })
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=CROFT_GIT_HASH={hash}");
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
