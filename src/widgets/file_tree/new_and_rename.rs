//! New File, New Folder and Rename: where a new entry goes, checking the name typed,
//! creating it, and renaming, a respelling that only changes case included.

use super::*;

/// Decide where a "New File" / "New Folder" should be created relative to the
/// node the user right-clicked on.
///
/// * Right-click on a directory → create *inside* that directory.
/// * Right-click on a file       → create as a *sibling* (in the file's parent).
/// * Right-click on empty space  → create in the workspace root.
pub fn create_target_dir_for(node: Option<&Node>, root: &Path) -> PathBuf {
    let Some(node) = node else {
        return root.to_path_buf();
    };
    if node.is_dir {
        node.path.clone()
    } else {
        node.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf())
    }
}

/// Validate a name typed into the New File / New Folder prompt.
/// Returns Ok(()) if it can become a single child entry safely.
pub fn validate_new_name(name: &str) -> Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name cannot be empty");
    }
    if trimmed == "." || trimmed == ".." {
        return Err("name cannot be '.' or '..'");
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("name cannot contain slashes");
    }
    if trimmed.contains('\0') {
        return Err("name cannot contain NUL");
    }
    Ok(())
}

/// Create an empty file `name` inside `parent`. Errors if it already exists.
pub fn create_file_in(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let target = parent.join(name);
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    Ok(target)
}

/// Rename the entry at `old_path` to `new_name` within `parent`. Validates
/// `new_name` via `validate_new_name`, refuses to overwrite an existing
/// entry, and returns the new absolute path on success. A no-op rename
/// (same name) returns Ok with the original path unchanged so the user
/// can hit Enter on the prompt without typing.
pub fn rename_in(parent: &Path, old_path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let target = rename_target(parent, old_path, new_name)?;
    if target != old_path {
        std::fs::rename(old_path, &target)?;
    }
    Ok(target)
}

/// Whether renaming `old_path` to `new_name` in `parent` only respells the
/// same entry on a case-insensitive volume (macOS by default), where
/// `README.md` "exists" while `readme.md` is renamed to it. True only when
/// the directory does not list `new_name` as an entry of its own (so a
/// separate file, or a hard link to the same file, is still a clash) and
/// the name resolves to the entry being renamed.
pub(super) fn is_case_only_respelling(parent: &Path, old_path: &Path, new_name: &str) -> bool {
    let listed = std::fs::read_dir(parent).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name() == std::ffi::OsStr::new(new_name))
    });
    if listed {
        return false;
    }
    let target = parent.join(new_name);
    match (
        std::fs::symlink_metadata(&target),
        std::fs::symlink_metadata(old_path),
    ) {
        #[cfg(unix)]
        (Ok(a), Ok(b)) => {
            use std::os::unix::fs::MetadataExt;
            a.dev() == b.dev() && a.ino() == b.ino()
        }
        // No inode to compare: a name the directory does not list that
        // still resolves can only be the same entry respelled.
        #[cfg(not(unix))]
        (Ok(_), Ok(_)) => true,
        _ => false,
    }
}

/// Where [`rename_in`] would put `old_path`, with the same validation and
/// without touching the disk, so a caller can ask language servers about
/// the rename before it happens (#610).
pub fn rename_target(parent: &Path, old_path: &Path, new_name: &str) -> std::io::Result<PathBuf> {
    let trimmed = new_name.trim();
    if let Err(msg) = validate_new_name(trimmed) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg));
    }
    let target = parent.join(trimmed);
    if target != old_path && target.exists() && !is_case_only_respelling(parent, old_path, trimmed)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    Ok(target)
}

/// Create directory `name` inside `parent`. Errors if it already exists.
pub fn create_folder_in(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let target = parent.join(name);
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    std::fs::create_dir(&target)?;
    Ok(target)
}
