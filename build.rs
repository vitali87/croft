use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CROFT_REPOSITORY_REMOTE");
    println!("cargo:rerun-if-changed=.git/config");

    let remote = std::env::var("CROFT_REPOSITORY_REMOTE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_origin_url);

    if let Some(remote) = remote {
        println!("cargo:rustc-env=CROFT_REPOSITORY_REMOTE={}", remote.trim());
    }
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
