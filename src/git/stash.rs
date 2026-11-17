//! The stash: stashing (everything, with untracked files, or only what is staged), and
//! listing, showing, applying, popping and dropping entries.

use super::*;

/// Stash the working tree (`git stash push`). Tracked changes are saved
/// and the working tree reverts to HEAD, exactly like VS Code's Stash.
/// A `message` names the stash (`-m`, #1581); without one git calls it
/// `WIP on <branch>: <commit>`.
pub fn stash_push(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &[], message)
}

/// Which of Source Control's three stash commands is asking for a
/// message (#1581).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StashKind {
    /// `git stash push`.
    Tracked,
    /// `git stash push -u`.
    IncludeUntracked,
    /// `git stash push --staged`.
    Staged,
}

/// `git stash push <extra> [-m <message>]`, a blank message counting as
/// none.
pub(super) fn stash_push_args(
    root: &Path,
    extra: &[&str],
    message: Option<&str>,
) -> Result<String, String> {
    let mut args = vec!["stash", "push"];
    args.extend_from_slice(extra);
    if let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) {
        args.extend(["-m", message]);
    }
    run_mutation(root, &args)
}

/// Restore and drop the most recent stash (`git stash pop`). A pop
/// conflict surfaces git's verbatim message so the user can resolve it.
pub fn stash_pop(root: &Path) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "pop"])
}

// --- Stash (full set) ----------------------------------------------------

/// Stash including untracked files (`git stash push -u`).
pub fn stash_push_untracked(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &["-u"], message)
}

/// Stash only the staged changes (`git stash push --staged`).
pub fn stash_push_staged(root: &Path, message: Option<&str>) -> Result<String, String> {
    stash_push_args(root, &["--staged"], message)
}

/// One entry in the stash list, for the apply/pop/drop pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashInfo {
    /// Stash index (`stash@{N}` is built from this).
    pub index: usize,
    /// The human description git stores (`WIP on main: …`).
    pub message: String,
    /// How long ago it was made (`5 minutes ago`, #1581).
    pub age: String,
    /// The files it holds, untracked ones included (#1581), so two stashes
    /// on the same commit can be told apart.
    pub files: Vec<String>,
}

impl StashInfo {
    /// The picker row (#1581): `stash@{N}: <message> · <age> · <files>`,
    /// naming at most three files.
    pub fn label(&self) -> String {
        let mut label = format!("stash@{{{}}}: {}", self.index, self.message);
        if !self.age.is_empty() {
            label.push_str(&format!(" \u{b7} {}", self.age));
        }
        if !self.files.is_empty() {
            let shown: Vec<&str> = self.files.iter().take(3).map(String::as_str).collect();
            label.push_str(&format!(" \u{b7} {}", shown.join(", ")));
            if self.files.len() > 3 {
                label.push_str(&format!(" +{}", self.files.len() - 3));
            }
        }
        label
    }
}

/// List stashes newest-first (`git stash list`), each with its age and
/// files. Two git processes however many stashes there are, since the
/// pickers call this on the UI thread: one `git stash list --name-only`
/// for the tracked files (diffed against the commit the stash was made
/// on) and one `git show` over every stash's untracked-files commit.
pub fn list_stashes(root: &Path) -> Result<Vec<StashInfo>, String> {
    let raw = run_git(
        root,
        &[
            "stash",
            "list",
            "-m",
            "--name-only",
            "--format=%x1e%gd%x1f%cr%x1f%P%x1f%gs",
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    // The third parent, when there is one, holds the untracked files.
    let mut untracked_commits: Vec<(usize, String)> = Vec::new();
    for record in raw.split('\x1e') {
        let mut lines = record.lines();
        let Some(header) = lines.next().filter(|h| !h.is_empty()) else {
            continue;
        };
        let mut parts = header.split('\x1f');
        let reflog = parts.next().unwrap_or("");
        let age = parts.next().unwrap_or("").to_string();
        let parents = parts.next().unwrap_or("");
        let message = parts.next().unwrap_or("").to_string();
        // reflog looks like "stash@{2}" — pull the integer out.
        let index = reflog
            .trim_start_matches("stash@{")
            .trim_end_matches('}')
            .parse::<usize>()
            .unwrap_or(out.len());
        if let Some(untracked) = parents.split_whitespace().nth(2) {
            untracked_commits.push((out.len(), untracked.to_string()));
        }
        let files = lines
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        out.push(StashInfo {
            index,
            message,
            age,
            files,
        });
    }
    if !untracked_commits.is_empty() {
        let mut args = vec!["show", "--root", "--name-only", "--format=%x1e%H"];
        args.extend(untracked_commits.iter().map(|(_, hash)| hash.as_str()));
        if let Ok(raw) = run_git(root, &args) {
            let mut by_hash: std::collections::HashMap<&str, Vec<String>> =
                std::collections::HashMap::new();
            for record in raw.split('\x1e') {
                let mut lines = record.lines();
                let Some(hash) = lines.next().filter(|h| !h.is_empty()) else {
                    continue;
                };
                let names = lines
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                by_hash.insert(hash.trim(), names);
            }
            for (pos, hash) in &untracked_commits {
                if let Some(names) = by_hash.remove(hash.as_str()) {
                    let files = &mut out[*pos].files;
                    files.extend(names);
                    files.sort();
                    files.dedup();
                }
            }
        }
    }
    Ok(out)
}

/// A stash's changes as a patch against the commit it was made on,
/// untracked files included (`git stash show --stat --patch
/// --include-untracked stash@{N}`), for View Stash (#1581).
pub fn show_stash(root: &Path, index: usize) -> Result<String, String> {
    diff_text(
        root,
        &[
            "stash",
            "show",
            "--no-color",
            "--no-ext-diff",
            "--stat",
            "--patch",
            "--include-untracked",
            &format!("stash@{{{index}}}"),
        ],
    )
}

/// Apply a stash without dropping it (`git stash apply stash@{N}`).
pub fn stash_apply(root: &Path, index: usize) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "apply", &format!("stash@{{{index}}}")])
}

/// Apply and drop a specific stash (`git stash pop stash@{N}`).
pub fn stash_pop_at(root: &Path, index: usize) -> Result<String, String> {
    run_mutation_keeping_conflicts(root, &["stash", "pop", &format!("stash@{{{index}}}")])
}

/// Drop a stash without applying it (`git stash drop stash@{N}`).
pub fn stash_drop(root: &Path, index: usize) -> Result<String, String> {
    run_mutation(root, &["stash", "drop", &format!("stash@{{{index}}}")])
}

/// The commit a stash entry records (`git rev-parse stash@{N}`). Unlike its
/// index, it keeps naming the same stash while others are pushed or dropped.
pub fn stash_oid(root: &Path, index: usize) -> Result<String, String> {
    run_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("stash@{{{index}}}"),
        ],
    )
    .map_err(|e| e.to_string())
}

/// Drop the stash whose commit is `oid`, at whatever index it holds now: a
/// stash pushed or dropped since it was picked shifts every index, and the
/// old one would name a different stash. Refuses when it is gone.
pub fn stash_drop_oid(root: &Path, oid: &str) -> Result<String, String> {
    stash_drop(root, stash_index_of(root, oid)?)
}

/// Restore and drop the stash whose commit is `oid`, at whatever index it
/// holds now, as [`stash_drop_oid`] drops one: a stash pushed since, by a
/// checkout hook or another tool, would make a plain `git stash pop` restore
/// that one instead. Refuses when it is gone.
pub fn stash_pop_oid(root: &Path, oid: &str) -> Result<String, String> {
    stash_pop_at(root, stash_index_of(root, oid)?)
}

/// The index the stash whose commit is `oid` holds now, or an error when it
/// is no longer in the stash list.
pub(super) fn stash_index_of(root: &Path, oid: &str) -> Result<usize, String> {
    let list = run_git(root, &["stash", "list", "--format=%H"]).map_err(|e| e.to_string())?;
    list.lines()
        .position(|hash| hash.trim() == oid)
        .ok_or_else(|| String::from("that stash is no longer in the stash list"))
}
