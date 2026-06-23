use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CROFT_REPOSITORY_REMOTE");
    println!("cargo:rerun-if-changed=.git/config");
    // Re-bake when HEAD moves (new commit / checkout / reset) so the welcome
    // panel's "this release" list matches the commit the binary was built from.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");

    let remote = std::env::var("CROFT_REPOSITORY_REMOTE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_origin_url);

    if let Some(remote) = remote {
        println!("cargo:rustc-env=CROFT_REPOSITORY_REMOTE={}", remote.trim());
    }

    // Bake the last few commits into the binary so the welcome panel shows what
    // is IN this build, never live commits fetched from the remote on every
    // launch (misleading, and a source of HTTP 429s). A `cargo:rustc-env` value
    // can't contain a newline, so each inter-record newline becomes \x1e;
    // `git::parse_baked_commits` swaps it back. Absent / non-git tree -> unset.
    if let Some(commits) = git_release_commits() {
        println!("cargo:rustc-env=CROFT_RELEASE_COMMITS={commits}");
    }
}

fn git_release_commits() -> Option<String> {
    let output = Command::new("git")
        .args([
            "log",
            "--no-merges",
            "--pretty=format:%h%x09%H%x09%ct%x09%s",
            "-n",
            "5",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let log = String::from_utf8_lossy(&output.stdout);
    let trimmed = log.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.replace('\n', "\u{1e}"))
}

fn git_origin_url() -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if remote.is_empty() {
        None
    } else {
        Some(remote)
    }
}
