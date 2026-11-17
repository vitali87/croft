//! A file's contents at HEAD, at a commit or at an index stage, a conflict's three stages
//! merged again in diff3 style, and the clean/smudge filter (Git LFS) a path goes through.

use super::*;

/// The clean/smudge filter `rel_path` goes through (`lfs` for a Git LFS
/// file), from `git check-attr filter`. `None` when no filter applies.
pub fn path_filter(root: &Path, rel_path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-attr", "filter", "--", rel_path])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text.trim_end().rsplit(": ").next()?;
    match value {
        "" => None,
        // check-attr prints these states and the same-named values alike
        // (`filter` and `filter=set` both read `set`): only a configured
        // driver makes one a filter.
        "unspecified" | "unset" | "set" if !filter_driver_configured(root, value) => None,
        name => Some(name.to_string()),
    }
}

/// Whether git config defines a `filter.<name>` driver.
pub(super) fn filter_driver_configured(root: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get-regexp"])
        .arg(format!(
            r"^filter\.{}\.(clean|smudge|process)$",
            regex::escape(name)
        ))
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Result of an attempted commit. `Ok(summary)` carries git's stdout/stderr
/// summary (e.g. "[main 4a5b6c7] message"); `Err` carries git's error
/// Read the HEAD-committed contents of `rel_path` (workspace-relative)
/// via `git show HEAD:<rel_path>`. Used by the Source Control panel to
/// surface a side-by-side diff between the working tree and HEAD when
/// the user clicks a Modified entry. Returns the raw bytes as a String;
/// non-UTF8 content (binary file) is reported as an error so the caller
/// can fall back to opening the file directly.
pub fn read_file_at_head(root: &Path, rel_path: &str) -> Result<String, String> {
    let bytes = read_bytes_at_head(root, rel_path)?;
    String::from_utf8(bytes).map_err(|_| format!("{rel_path} at HEAD is not UTF-8"))
}

/// The raw bytes of `rel_path` at HEAD (`git show HEAD:<rel_path>`), for a
/// caller that decodes them itself, with the encoding the file's tab uses.
pub fn read_bytes_at_head(root: &Path, rel_path: &str) -> Result<Vec<u8>, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let spec = format!("HEAD:{rel_path}");
    let output = show_blob(Path::new(path_str), &spec, rel_path)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "git show {spec} failed with code {:?}",
                output.status.code()
            )
        } else {
            stderr
        });
    }
    Ok(output.stdout)
}

/// The `git` command that prints the blob `spec` names, for `rel_path`.
///
/// A filtered file (Git LFS) is stored in its clean form, a pointer, while
/// the working tree holds the smudged content: such a blob is read through
/// the filter (`cat-file --filters`), so every view of a past version shows
/// the file and not the pointer (#1238, #1338). Anything else is read with
/// `git show`, byte for byte as git stores it.
pub(super) fn show_blob(root: &Path, spec: &str, rel_path: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root);
    match path_filter(root, rel_path) {
        Some(_) => cmd.args(["cat-file", "--filters"]),
        None => cmd.arg("show"),
    };
    cmd.arg(spec);
    cmd
}

/// The contents of `rel_path` (relative to `root`) at revision `rev`, via
/// `git show <rev>:./<rel_path>` (through the filter for a filtered file). The `./` makes the path relative to
/// `root` rather than to the repository top, so a workspace opened in a
/// subdirectory of a repo still reads the right file. `Err` when the file
/// did not exist at that revision or is not UTF-8.
pub fn read_file_at_rev(root: &Path, rev: &str, rel_path: &str) -> Result<String, String> {
    let spec = format!("{rev}:./{rel_path}");
    let output = show_blob(root, &spec, rel_path)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{rel_path} at {rev} is not UTF-8"))
}

/// Read one index stage of an unmerged path via `git show :N:<rel_path>`
/// — 1 = common ancestor (base), 2 = ours, 3 = theirs. The merge
/// editor's input source (#253). Errors when the stage does not exist
/// (e.g. no base for an added-by-both conflict, or the path is not
/// actually unmerged) or the blob is not UTF-8; the caller decides
/// whether that's fatal or means "empty side".
pub fn read_file_at_stage(root: &Path, rel_path: &str, stage: u8) -> Result<String, String> {
    let path_str = root
        .to_str()
        .ok_or_else(|| "non-utf8 workspace path".to_string())?;
    let spec = format!(":{stage}:{rel_path}");
    let output = show_blob(Path::new(path_str), &spec, rel_path)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!(
                "git show {spec} failed with code {:?}",
                output.status.code()
            )
        } else {
            stderr
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|_| format!("{rel_path} at stage {stage} is not UTF-8"))
}

/// The unmerged `rel_path`'s three index stages merged again by
/// `git merge-file --diff3`, whose conflicts keep each side whole where
/// git's default style trims what the sides share (the inline Accept
/// Both's source, see `merge::untrimmed_both`). Errors when a stage is
/// missing (not unmerged, or an add/add with no base) or git fails.
pub fn remerge_diff3(root: &Path, rel_path: &str) -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let sides = [
        ("ours", read_file_at_stage(root, rel_path, 2)?),
        ("base", read_file_at_stage(root, rel_path, 1)?),
        ("theirs", read_file_at_stage(root, rel_path, 3)?),
    ];
    // A directory of our own, made fresh: `create_dir_all` would take one
    // another local user made first at this guessable name, and the writes
    // below would follow the symlinks they left in it.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!(
        "croft-remerge-{}-{}-{nanos}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).map_err(|e| e.to_string())?;
    let merged = (|| {
        for (name, text) in &sides {
            std::fs::write(dir.join(name), text).map_err(|e| e.to_string())?;
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["merge-file", "-p", "-q", "--diff3"])
            .args(sides.iter().map(|(name, _)| dir.join(name)))
            .output()
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        // The exit code is the conflict count (at most 127); an error
        // exits negative, which reads as 255.
        match output.status.code() {
            Some(0..=127) => String::from_utf8(output.stdout)
                .map_err(|_| format!("{rel_path} merged is not UTF-8")),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    merged
}
