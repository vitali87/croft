//! Pasting and dropping: a destination name that collides with nothing, refusing a
//! folder into itself, and moving or copying across file systems.

use super::*;

/// Suggest a non-colliding destination path for `source` placed inside
/// `dest_dir`. If a sibling with the source's basename already exists,
/// appends ` copy`, ` copy 2`, … until a free name is found, mirroring how
/// macOS Finder de-duplicates pasted names. Returns the resolved path; the
/// caller still has to perform the actual move/copy syscall.
pub fn unique_destination_in(dest_dir: &Path, source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = source.extension().map(|s| s.to_string_lossy().into_owned());
    let original_name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = dest_dir.join(&original_name);
    if !candidate.exists() {
        return candidate;
    }
    for n in 1.. {
        let suffix = if n == 1 {
            String::from(" copy")
        } else {
            format!(" copy {n}")
        };
        let name = match &ext {
            Some(e) if !stem.is_empty() => format!("{stem}{suffix}.{e}"),
            Some(e) => format!("{suffix}.{e}"),
            None => format!("{stem}{suffix}"),
        };
        candidate = dest_dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// Reject paste/drop operations that would move a directory into itself or
/// a descendant of itself (which would either error or, worse, produce an
/// infinite-recursion copy on filesystems that follow symlinks).
pub fn is_descendant_or_same(target: &Path, source: &Path) -> bool {
    let canon_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let canon_source = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    canon_target == canon_source || canon_target.starts_with(&canon_source)
}

/// Whether `source` already sits directly in `dir`, so moving it there moves
/// nothing (#1174). A source that no longer exists (deleted or renamed since
/// it was cut) is not in place: it takes the ordinary path, which reports it
/// missing. A dangling symlink still exists, as the link itself.
pub fn is_directly_in(dir: &Path, source: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    source.symlink_metadata().is_ok()
        && source
            .parent()
            .is_some_and(|parent| canon(parent) == canon(dir))
}

/// Move `source` to a fresh path inside `dest_dir`. Falls back to
/// copy-then-remove when the source and destination live on different
/// filesystems and `std::fs::rename` returns `EXDEV`. Returns the final
/// destination path on success.
pub fn move_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot move {} into itself", source.display()),
        ));
    }
    // Already there: a fresh name would rename it to "<name> copy" (#1174).
    if is_directly_in(dest_dir, source) {
        return Ok(source.to_path_buf());
    }
    let dest = unique_destination_in(dest_dir, source);
    if std::fs::rename(source, &dest).is_ok() {
        return Ok(dest);
    }
    copy_recursive(source, &dest)?;
    remove_recursive(source)?;
    Ok(dest)
}

/// Recursively copy `source` to `dest`. `dest` must not already exist.
pub fn copy_into(dest_dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    if is_descendant_or_same(dest_dir, source) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("cannot copy {} into itself", source.display()),
        ));
    }
    let dest = unique_destination_in(dest_dir, source);
    copy_recursive(source, &dest)?;
    Ok(dest)
}

pub(super) fn copy_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        std::fs::create_dir(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let from = entry.path();
            let to = dest.join(entry.file_name());
            copy_recursive(&from, &to)?;
        }
        Ok(())
    } else if ft.is_symlink() {
        let link_target = std::fs::read_link(source)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&link_target, dest)?;
        }
        #[cfg(not(unix))]
        {
            let _ = link_target;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlink copy not supported on this platform",
            ));
        }
        Ok(())
    } else {
        std::fs::copy(source, dest).map(|_| ())
    }
}

pub(super) fn remove_recursive(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}
