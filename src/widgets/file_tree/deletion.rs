//! Delete: which rows may be deleted, and moving one or many paths to the system's
//! trash, recoverably.

use super::*;

/// Decide whether a given node may be deleted via the explorer's right-click
/// menu, returning the path to delete. Refuses for the workspace root and
/// returns `None` if `node` is None (i.e. right-click on empty tree space).
pub fn delete_target_for(node: Option<&Node>, root: &Path) -> Option<PathBuf> {
    let n = node?;
    // Depth 0 is the root-row marker: with multiple workspace roots
    // (#145) every section row is protected, not just the primary.
    if n.depth == 0 || n.path == root {
        return None;
    }
    let canon_root = root.canonicalize().ok();
    let canon_target = n.path.canonicalize().ok();
    if canon_root.is_some() && canon_target == canon_root {
        return None;
    }
    Some(n.path.clone())
}

/// Move `path` to the OS trash (recoverable) rather than unlinking it.
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        trash::delete(path).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        android_trash::trash_one(path)
    }
}

/// Bulk-trash a batch of paths in a single OS call. On macOS this routes
/// through Finder via one AppleScript so the system trash sound plays once
/// for the whole batch instead of once per file. On Linux/Windows the
/// underlying `trash::delete_all` already groups the request. Android has no
/// OS trash, so each entry is moved into the freedesktop home trash directly.
pub fn move_to_trash_bulk(paths: &[PathBuf]) -> std::io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::Finder);
        ctx.delete_all(paths)
            .map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        trash::delete_all(paths).map_err(|e| std::io::Error::other(format!("{e}")))
    }
    #[cfg(target_os = "android")]
    {
        for p in paths {
            android_trash::trash_one(p)?;
        }
        Ok(())
    }
}
