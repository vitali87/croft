//! Remotes: pulling, pushing and publishing, fetching and cloning, adding and removing
//! remotes, and a link to a file on the remote's website.

use super::*;

/// Push the current branch to its upstream. Used by Commit & Push
/// (Cmd+Enter in the SC message box). Returns the trimmed stdout/stderr
/// summary verbatim so the panel can surface the host's full message —
/// `git push` typically reports the ref update on stderr even on success.
pub fn push_current_branch(root: &Path) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let output = Git::new()
        .at(path_str)
        .arg("push")
        .never_prompting()
        .into_command()
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let summary = if !stderr.is_empty() { stderr } else { stdout };
        Ok(summary)
    } else {
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git push failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// Pull the current branch from its upstream (`git pull`). Sibling of
/// `push_current_branch`; surfaced by the commit dropdown's "Pull" item
/// and used by `sync` below. Returns git's verbatim summary ("Already
/// up to date." / the fast-forward range) so the panel can echo it.
pub fn pull_current_branch(root: &Path) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["pull"])
}

// --- Fetch / Clone -------------------------------------------------------

/// Fetch from all remotes without merging (`git fetch --all --prune`).
/// VS Code's "Fetch" updates remote-tracking refs so ahead/behind counts
/// refresh; `--prune` drops refs deleted on the remote.
pub fn fetch_all(root: &Path) -> Result<String, String> {
    run_mutation(root, &["fetch", "--all", "--prune"])
}

/// Derive the directory name a clone of `url` would land in: the final
/// path segment with any trailing `.git` and slash stripped. Returns
/// `None` when the URL has no usable segment.
pub fn clone_dir_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let seg = trimmed.rsplit(['/', ':']).next()?.trim();
    let name = seg.strip_suffix(".git").unwrap_or(seg).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Clone `url` into `parent/<repo-name>` and return the new repo path.
/// The Source Control "Clone" flow prompts for the URL, clones beside the
/// current workspace, then re-roots croft into the clone.
pub fn clone_into(parent: &Path, url: &str) -> Result<PathBuf, String> {
    let name =
        clone_dir_name(url).ok_or_else(|| format!("can't derive a folder name from {url}"))?;
    let dest = parent.join(&name);
    // run_mutation runs with `-C <parent>`, so the bare `<name>` clones into
    // parent/<name>.
    // `--`: a URL starting with `-` (`--upload-pack=<command>`) is
    // otherwise an option, and that one runs a command.
    run_mutation(parent, &["clone", "--", url, &name]).map(|_| dest)
}

// --- Pull / Push variants ------------------------------------------------

/// Pull with rebase instead of merge (`git pull --rebase`).
pub fn pull_rebase(root: &Path) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["pull", "--rebase"])
}

/// Force-push the current branch, but only if the remote hasn't advanced
/// past what we last saw (`git push --force-with-lease`). Safer than a
/// bare `--force`; VS Code's "Push (Force)" uses the same lease guard.
pub fn push_force(root: &Path) -> Result<String, String> {
    run_mutation(root, &["push", "--force-with-lease"])
}

/// Push the current branch to a specific remote (`git push <remote>`).
pub fn push_to_remote(root: &Path, remote: &str) -> Result<String, String> {
    run_mutation(root, &["push", remote])
}

/// Publish the current branch: push it and set upstream tracking
/// (`git push -u <remote> <branch>`, the remote chosen by
/// [`publish_remote`]). Used when a local branch has no upstream yet.
pub fn publish_branch(root: &Path, branch: &str) -> Result<String, String> {
    let remote = publish_remote(root)?;
    run_mutation(root, &["push", "-u", &remote, branch])?;
    Ok(format!("{branch} to {remote}"))
}

/// The current branch when it has no upstream to push to or pull from
/// (`@{u}` does not resolve), the case `git push` refuses with "has no
/// upstream branch". None on a tracked branch and on a detached HEAD.
pub fn unpublished_branch(root: &Path) -> Option<String> {
    let branch = run_git(root, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .and_then(parse_branch)?;
    let tracked = run_git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok();
    (!tracked).then_some(branch)
}

/// The remote an unpublished branch goes to: `remote.pushDefault` when
/// set, else `origin`, else the only remote. With several remotes and
/// none of those, picking one would be a guess, so it is an error.
pub(super) fn publish_remote(root: &Path) -> Result<String, String> {
    match default_push_remote(root)? {
        Some(remote) => Ok(remote),
        None if run_git(root, &["remote"]).is_ok_and(|r| r.is_empty()) => {
            Err(String::from("no remote to publish this branch to"))
        }
        None => Err(String::from(
            "branch has no upstream and several remotes, none of them origin: set remote.pushDefault to pick one",
        )),
    }
}

/// `remote.pushDefault`, else the only remote, else `origin` among
/// several; `None` when there is no remote or several and none of those.
pub(super) fn default_push_remote(root: &Path) -> Result<Option<String>, String> {
    if let Ok(remote) = run_git(root, &["config", "--get", "remote.pushDefault"])
        && !remote.is_empty()
    {
        return Ok(Some(remote));
    }
    let listed = run_git(root, &["remote"]).map_err(|e| e.to_string())?;
    let remotes: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    Ok(match remotes.as_slice() {
        [only] => Some((*only).to_string()),
        several if several.contains(&"origin") => Some(String::from("origin")),
        _ => None,
    })
}

/// Push the current branch, publishing it first if it has no upstream
/// (`git push -u <remote> <branch>`), as VS Code's Push and Sync do. A
/// plain `git push` would refuse with "has no upstream branch".
pub fn push_or_publish(root: &Path) -> Result<String, String> {
    let Some(branch) = unpublished_branch(root) else {
        return push_current_branch(root);
    };
    publish_branch(root, &branch).map(|summary| format!("published {summary}"))
}

// --- Remotes -------------------------------------------------------------

/// A configured remote: its name and fetch URL, for the Remote submenu's
/// pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// List configured remotes (`git remote -v`), de-duplicated to one row per
/// remote (the fetch URL).
pub fn list_remotes(root: &Path) -> Result<Vec<RemoteInfo>, String> {
    let raw = run_git(root, &["remote", "-v"]).map_err(|e| e.to_string())?;
    let mut out: Vec<RemoteInfo> = Vec::new();
    for line in raw.lines() {
        // Format: "<name>\t<url> (fetch|push)"
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
            continue;
        };
        if out.iter().any(|r| r.name == name) {
            continue;
        }
        out.push(RemoteInfo {
            name: name.to_string(),
            url: url.to_string(),
        });
    }
    Ok(out)
}

/// Add a remote (`git remote add <name> <url>`).
pub fn add_remote(root: &Path, name: &str, url: &str) -> Result<String, String> {
    run_mutation(root, &["remote", "add", name, url])
}

/// Remove a remote (`git remote remove <name>`).
pub fn remove_remote(root: &Path, name: &str) -> Result<String, String> {
    run_mutation(root, &["remote", "remove", name])
}

/// Which revision a shared link names (#1610): HEAD's commit for Git: Copy
/// Permalink, which keeps pointing at these lines as the branch moves, or
/// the branch for Git: Copy Head Link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRev {
    Commit,
    Branch,
}

/// The web page for `file` at `rev`, scrolled to `lines` (1-based,
/// inclusive), on the current branch's remote, else `origin`, else the only
/// remote (#1610). `Err` says why there is no link rather than handing out
/// one that would not open: no repository or remote, a remote that is not
/// GitHub, GitLab or Bitbucket, a file that was never committed, or a
/// commit or branch that is not on the remote yet.
pub fn share_link(
    file: &Path,
    lines: Option<(usize, usize)>,
    rev: LinkRev,
) -> Result<String, String> {
    let dir = file.parent().ok_or("the file has no folder")?;
    let top = repo_toplevel(dir).ok_or("the file is not in a git repository")?;
    let rel = file
        .canonicalize()
        .ok()
        .zip(top.canonicalize().ok())
        .and_then(|(f, t)| f.strip_prefix(t).ok().map(Path::to_path_buf))
        .ok_or("the file is not in a git repository")?;
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let branch = run_git(&top, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let config = |key: String| run_git(&top, &["config", "--get", &key]).ok();
    let remotes = run_git(&top, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes.lines().collect();
    let remote = branch
        .as_ref()
        .and_then(|b| config(format!("branch.{b}.remote")))
        .filter(|r| remotes.contains(&r.as_str()))
        .or_else(|| remotes.contains(&"origin").then(|| String::from("origin")))
        .or_else(|| (remotes.len() == 1).then(|| remotes[0].to_string()))
        .ok_or("the repository has no remote to link to")?;
    let url = run_git(&top, &["remote", "get-url", &remote])
        .map_err(|_| format!("remote {remote} has no URL"))?;
    let repo = crate::permalink::web_repo(&url)
        .ok_or_else(|| format!("{remote} ({url}) is not a GitHub, GitLab or Bitbucket remote"))?;
    // The file must be in HEAD's tree, which the linked commit or branch
    // contains; being staged is not enough for the remote to show it.
    if run_git(&top, &["cat-file", "-e", &format!("HEAD:{rel}")]).is_err() {
        return Err(format!(
            "{rel} is not committed, so {remote} has no page for it"
        ));
    }
    let rev = match rev {
        LinkRev::Commit => {
            let sha = run_git(&top, &["rev-parse", "HEAD"])
                .map_err(|_| String::from("the repository has no commits"))?;
            let pattern = format!("{remote}/*");
            let pushed = run_git(
                &top,
                &["branch", "-r", "--contains", &sha, "--list", &pattern],
            )
            .is_ok_and(|out| !out.is_empty());
            if !pushed {
                return Err(format!(
                    "commit {} is not on {remote} yet, so the link would not open: push it first",
                    &sha[..sha.len().min(7)]
                ));
            }
            sha
        }
        LinkRev::Branch => {
            let branch = branch.ok_or("HEAD is detached, so there is no branch to link to")?;
            // The branch's name on the remote, which its upstream may spell
            // differently; else the same name.
            let name = config(format!("branch.{branch}.merge"))
                .and_then(|m| m.strip_prefix("refs/heads/").map(str::to_string))
                .unwrap_or_else(|| branch.clone());
            let tracking = format!("refs/remotes/{remote}/{name}");
            if run_git(&top, &["rev-parse", "--verify", "--quiet", &tracking]).is_err() {
                return Err(format!(
                    "branch {name} is not on {remote} yet, so the link would not open: push it first"
                ));
            }
            // A tracking branch behind HEAD would show older lines.
            if run_git(&top, &["merge-base", "--is-ancestor", "HEAD", &tracking]).is_err() {
                return Err(format!(
                    "branch {name} on {remote} is behind your commits, so the link would show older lines: push it first"
                ));
            }
            name
        }
    };
    Ok(crate::permalink::file_url(&repo, &rev, &rel, lines))
}
