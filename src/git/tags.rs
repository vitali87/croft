//! Tags: listing, creating and deleting them, here and on a remote, and pushing them.

use super::*;

/// The remote tags are pushed to and deleted from (#1582): the one a new
/// branch is published to, by the same rules.
pub fn tag_remote(root: &Path) -> Result<String, String> {
    match default_push_remote(root)? {
        Some(remote) => Ok(remote),
        None if run_git(root, &["remote"]).is_ok_and(|r| r.is_empty()) => {
            Err(String::from("no remote to push tags to"))
        }
        None => Err(String::from(
            "several remotes, none of them origin: set remote.pushDefault to pick one",
        )),
    }
}

// --- Tags ----------------------------------------------------------------

/// List tags newest-first (`git tag --sort=-creatordate`).
pub fn list_tags(root: &Path) -> Result<Vec<String>, String> {
    let raw = run_git(root, &["tag", "--sort=-creatordate"]).map_err(|e| e.to_string())?;
    Ok(raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Create a tag at HEAD: annotated (`git tag -a <name> -m <message>`)
/// when there is a message, as VS Code's Create Tag makes one (#1582),
/// else lightweight (`git tag <name>`).
pub fn create_tag(root: &Path, name: &str, message: Option<&str>) -> Result<String, String> {
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => run_mutation(root, &["tag", "-a", name, "-m", message]),
        None => run_mutation(root, &["tag", name]),
    }
}

/// Push every local tag to `remote` (`git push <remote> --tags`, #1582).
/// A plain push sends none, so a tag made here stayed local.
pub fn push_tags(root: &Path, remote: &str) -> Result<String, String> {
    run_mutation(root, &["push", remote, "--tags"])
}

/// The tags on `remote` (`git ls-remote --tags --refs`), by name (#1582).
pub fn list_remote_tags(root: &Path, remote: &str) -> Result<Vec<String>, String> {
    let raw = run_mutation(root, &["ls-remote", "--tags", "--refs", remote])?;
    Ok(parse_remote_tags(&raw))
}

/// The tag names in `git ls-remote --tags --refs` output; a peeled
/// `^{}` line, which `--refs` already drops, is the same tag again.
pub(super) fn parse_remote_tags(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| line.split_once('\t')?.1.strip_prefix("refs/tags/"))
        .filter(|tag| !tag.ends_with("^{}"))
        .map(str::to_string)
        .collect()
}

/// Delete tag `name` from `remote` (`git push <remote> --delete
/// refs/tags/<name>`, #1582); the local tag stays.
pub fn delete_remote_tag(root: &Path, remote: &str, name: &str) -> Result<String, String> {
    run_mutation(
        root,
        &["push", remote, "--delete", &format!("refs/tags/{name}")],
    )
}

/// Delete a tag (`git tag -d <name>`).
pub fn delete_tag(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["tag", "-d", name])
}
