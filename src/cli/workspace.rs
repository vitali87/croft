//! The `PATH` and `--open-file` arguments: the workspace root and the file to open, a
//! `file:line[:col]` location, a `.code-workspace` file's folders, and a file not written
//! yet.

use super::*;

/// Split the `PATH` argument into the workspace root and the file to open in
/// the editor. Handing croft a *file* roots the workspace at its parent and
/// opens the file, so `croft pitch_deck.tex` works and the macOS launcher can
/// pass a double-clicked document straight through. An explicit `--open-file`
/// always wins.
pub(super) fn resolve_workspace(
    path: &Path,
    open_file: Option<PathBuf>,
) -> Result<(PathBuf, Option<PathBuf>, Vec<PathBuf>)> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    resolve_workspace_from(path, open_file, &cwd)
}

/// `croft file:line[:col]` and `--open-file file:line[:col]` (#1606): the
/// `PATH` and `--open-file` arguments with a location suffix taken off, and
/// the 0-based (line, column) for the caret in the file that opens. An
/// explicit `--open-file` wins, its location with it. A relative one is
/// looked for under `cwd`, then under the workspace, as it is opened.
pub(super) fn split_launch_location(
    path: &Path,
    open_file: Option<PathBuf>,
    cwd: &Path,
) -> (PathBuf, Option<PathBuf>, Option<(usize, usize)>) {
    let (path, path_at) = match crate::view_ipc::split_location(cwd, path) {
        Some((file, at)) => (file, Some(at)),
        None => (path.to_path_buf(), None),
    };
    let Some(file) = open_file else {
        return (path, None, path_at);
    };
    let split = crate::view_ipc::split_location(cwd, &file).or_else(|| {
        launch_root(&cwd.join(&path)).and_then(|root| crate::view_ipc::split_location(&root, &file))
    });
    match split {
        Some((file, at)) => (path, Some(file), Some(at)),
        None => (path, Some(file), None),
    }
}

/// The folder a relative `--open-file` is looked for under once `path` is
/// resolved as [`resolve_workspace_from`] does: the directory itself, a
/// `.code-workspace` file's primary folder, or a file's parent. `None` when
/// there is none to search.
pub(super) fn launch_root(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.to_path_buf());
    }
    if crate::workspace::is_workspace_file(path) {
        return crate::workspace::parse_workspace_file(path)
            .ok()?
            .into_iter()
            .next();
    }
    path.is_file()
        .then(|| path.parent().map(Path::to_path_buf))
        .flatten()
}

/// An `--open-file` path made absolute: a relative one names a file under
/// `cwd` (as the shell typed it), else under the workspace `root`, so the
/// tab is keyed by the absolute path its language server tracks (#1193).
/// A path that exists is canonicalized like the workspace itself.
pub(super) fn resolve_open_file(open_file: PathBuf, root: &Path, cwd: &Path) -> PathBuf {
    let candidates = if open_file.is_absolute() {
        vec![open_file]
    } else {
        vec![cwd.join(&open_file), root.join(&open_file)]
    };
    candidates
        .iter()
        .find_map(|p| p.canonicalize().ok())
        .unwrap_or_else(|| candidates[candidates.len() - 1].clone())
}

/// The canonical folder and file name of `path` when `err` (its
/// canonicalize failure) says only its last component is missing: a file
/// not written yet. `None` for a path written as a folder (`new/`), one with
/// no name (`..`), or one whose folder is missing too.
pub(super) fn new_file_in_existing_dir(
    path: &Path,
    err: &std::io::Error,
) -> Option<(PathBuf, std::ffi::OsString)> {
    if err.kind() != std::io::ErrorKind::NotFound
        || path.as_os_str().as_encoded_bytes().ends_with(b"/")
    {
        return None;
    }
    let name = match path.components().next_back()? {
        std::path::Component::Normal(name) => name.to_os_string(),
        _ => return None,
    };
    let parent = path.parent()?.canonicalize().ok()?;
    parent.is_dir().then_some((parent, name))
}

/// [`resolve_workspace`] with the directory croft was launched from.
pub(super) fn resolve_workspace_from(
    path: &Path,
    open_file: Option<PathBuf>,
    cwd: &Path,
) -> Result<(PathBuf, Option<PathBuf>, Vec<PathBuf>)> {
    let joined = cwd.join(path);
    let path = match joined.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            // A file not written yet, in a folder that exists: root at the
            // folder and open the file as a new buffer, as `code new.md` and
            // `vim new.md` do (#1157). A missing folder is still an error.
            if let Some((parent, name)) = new_file_in_existing_dir(&joined, &e) {
                let open_file = open_file.map(|f| resolve_open_file(f, &parent, cwd));
                return Ok((
                    parent.clone(),
                    open_file.or(Some(parent.join(name))),
                    Vec::new(),
                ));
            }
            return Err(e).with_context(|| format!("resolving workspace path {}", path.display()));
        }
    };
    if path.is_dir() {
        let open_file = open_file.map(|f| resolve_open_file(f, &path, cwd));
        return Ok((path, open_file, Vec::new()));
    }
    // A VS Code-compatible workspace file (#163): the first folder is the
    // primary root, the rest join as workspace folders — the file defines
    // the set exactly, so the automatic per-primary store is not merged.
    if crate::workspace::is_workspace_file(&path) {
        let folders = crate::workspace::parse_workspace_file(&path)
            .map_err(|e| anyhow::anyhow!("workspace file: {e}"))?;
        let mut it = folders.into_iter();
        let primary = it.next().expect("parse guarantees at least one folder");
        let open_file = open_file.map(|f| resolve_open_file(f, &primary, cwd));
        return Ok((primary, open_file, it.collect()));
    }
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?
        .to_path_buf();
    let open_file = open_file.map(|f| resolve_open_file(f, &parent, cwd));
    Ok((parent, open_file.or(Some(path)), Vec::new()))
}
