//! Staging, unstaging and removing paths, and discarding their changes, one at a time or
//! all at once.

use super::*;

/// Stage a single path. Convenience wrapper over `stage_paths` for the
/// inline "+" icon click path; multi-select staging goes through
/// `stage_paths` directly so all paths land in one atomic `git add`
/// invocation (the index.lock is held only once, so the background
/// status worker can't race us between adds).
pub fn stage_path(root: &Path, rel_path: &str) -> Result<(), String> {
    stage_paths(root, std::slice::from_ref(&rel_path.to_string()))
}

/// `git rm` one path: out of the index and off the disk. Resolves a
/// modify/delete conflict in favour of the deletion (#1244).
pub fn remove_path(root: &Path, rel_path: &str) -> Result<(), String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let out = Git::new()
        .at(path_str)
        .args(["--literal-pathspecs", "rm", "-q", "-f", "--"])
        .arg(rel_path)
        .into_command()
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Stage all listed paths in a single `git add -- p1 p2 ...` invocation.
/// Atomic with respect to the index lock — running N separate `git add`
/// commands lets the background `git status` worker grab the lock
/// between them and fail later adds with "Unable to create index.lock".
pub fn stage_paths(root: &Path, rel_paths: &[String]) -> Result<(), String> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    refuse_conflict_markers(root, Some(rel_paths))?;
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    // Literal: git reads a pathspec as a glob, so staging Next.js's
    // `pages/[id].tsx` also staged `pages/i.tsx` and untracked `pages/d.tsx`.
    let output = Git::new()
        .at(path_str)
        .args(["--literal-pathspecs", "add", "--"])
        .args(rel_paths)
        .into_command()
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        Err(if msg.is_empty() {
            format!("git add failed with code {:?}", output.status.code())
        } else {
            msg
        })
    }
}

/// Unstage a single path. Convenience wrapper over `unstage_paths` for
/// the inline "−" icon click on a staged row.
pub fn unstage_path(root: &Path, rel_path: &str) -> Result<(), String> {
    unstage_paths(root, std::slice::from_ref(&rel_path.to_string()))
}

/// Move all listed paths out of the index and back into the working tree,
/// in one `git reset -q HEAD -- p1 p2 …` invocation. The mirror of
/// `stage_paths`: `git reset HEAD` is what VS Code's git extension runs to
/// unstage, and unlike `git restore --staged` it also unstages files added
/// in a repo's very first commit. Atomic w.r.t. the index lock, so the
/// background status worker can't race us between paths.
pub fn unstage_paths(root: &Path, rel_paths: &[String]) -> Result<(), String> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = vec!["--literal-pathspecs", "reset", "-q", "HEAD", "--"];
    args.extend(rel_paths.iter().map(String::as_str));
    run_mutation(root, &args).map(|_| ())
}

/// Discard a single path. A Changes row restores the working tree from the
/// INDEX (`git checkout -- <path>`), so a staged part of the same file is
/// kept, as VS Code does; restoring from HEAD threw the staged work away
/// too. A Staged row restores both from HEAD. Untracked entries are removed
/// from disk. Destructive: callers
/// MUST confirm with the user before invoking — the Source Control panel
/// shows a Y/N modal before reaching this function.
pub fn discard_path(
    root: &Path,
    rel_path: &str,
    untracked: bool,
    staged: bool,
) -> Result<(), String> {
    if untracked {
        let abs = root.join(rel_path);
        if abs.is_dir() {
            // Porcelain folds a directory holding any untracked file into one
            // `?? dir/` row; `remove_dir_all` took the IGNORED files inside
            // with it (`.env`, `node_modules/`). `git clean` removes only
            // the untracked ones.
            run_mutation(
                root,
                &["--literal-pathspecs", "clean", "-fdq", "--", rel_path],
            )
            .map(|_| ())
        } else {
            std::fs::remove_file(&abs)
                .map_err(|e| format!("failed to remove {}: {e}", abs.display()))
        }
    } else {
        // `git checkout -- <submodule>` exits 0 and changes nothing, which
        // reported "Discarded" over a submodule still on its new commit
        // with its edits intact (#1235). Say so instead.
        if is_submodule(root, rel_path) {
            return Err(format!(
                "{rel_path} is a submodule; open it to discard its changes"
            ));
        }
        let path_str = root
            .to_str()
            .ok_or_else(|| "non-utf8 workspace path".to_string())?;
        let output = Git::new()
            .at(path_str)
            .args(["--literal-pathspecs", "checkout"])
            .args(staged.then_some("HEAD"))
            .args(["--", rel_path])
            .into_command()
            .output()
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !stderr.is_empty() { stderr } else { stdout };
            Err(if msg.is_empty() {
                format!("git checkout failed with code {:?}", output.status.code())
            } else {
                msg
            })
        }
    }
}

// --- Bulk staging --------------------------------------------------------

/// Stage every change, tracked and untracked (`git add -A`). Refused while
/// a conflicted file still holds its markers.
pub fn stage_all(root: &Path) -> Result<String, String> {
    refuse_conflict_markers(root, None)?;
    run_mutation(root, &["add", "-A"])
}

/// Unstage everything (`git reset -q HEAD`), moving the whole index back
/// into the working tree.
pub fn unstage_all(root: &Path) -> Result<String, String> {
    run_mutation(root, &["reset", "-q", "HEAD"])
}

/// Discard every tracked modification (`git checkout -- .`) — destructive,
/// the caller MUST confirm. Untracked files are left untouched (matching
/// the per-file discard, which deletes untracked only on explicit request).
pub fn discard_all_tracked(root: &Path) -> Result<String, String> {
    run_mutation(root, &["checkout", "--", "."])
}
