//! Finding a program croft starts (#2062, card 12 step 10): one rule for
//! what counts as one, and the scans every lookup makes with it.
//!
//! A program is what starting it would accept: a regular file with an
//! execute bit, links followed. A folder, or a file croft cannot run, is
//! passed over, as a shell passes it over for the next folder on `PATH`
//! (#2808). Taken as the program, it failed to start with "permission
//! denied" where the same tool ran in a terminal.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Whether `path` is a program that can be started: a regular file with an
/// execute bit, links followed. Where files carry no mode bits, any file.
pub(crate) fn runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `name` in the first of `dirs` where it is [`runnable`].
pub(crate) fn first_in<P: AsRef<Path>>(
    name: &str,
    dirs: impl IntoIterator<Item = P>,
) -> Option<PathBuf> {
    dirs.into_iter()
        .map(|dir| dir.as_ref().join(name))
        .find(|candidate| runnable(candidate))
}

/// `name` on the `PATH` value `path_var`: the first one that can run.
pub(crate) fn on_path_in(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    first_in(name, std::env::split_paths(path_var?))
}

/// `name` on croft's own `PATH`.
pub(crate) fn on_path(name: &str) -> Option<PathBuf> {
    on_path_in(name, std::env::var_os("PATH").as_deref())
}

/// Where Go's installers put `go` when croft's `PATH` lacks it (#2807): the
/// official installer's folder, then Homebrew's and the usual local prefix.
/// A launcher's `PATH` often has none of them.
const GO_HOMES: [&str; 3] = ["/usr/local/go/bin", "/opt/homebrew/bin", "/usr/local/bin"];

/// The `go` every feature uses (#2807): the Testing view's `go test`, the
/// Go language server's install and the Go debugger's all ask here, so a Go
/// that runs tests also installs gopls and delve.
pub(crate) fn go() -> Option<PathBuf> {
    go_in(
        std::env::var_os("PATH").as_deref(),
        &GO_HOMES.map(PathBuf::from),
    )
}

/// [`go`] on the `PATH` value `path_var`, then in `homes`.
pub(crate) fn go_in(path_var: Option<&OsStr>, homes: &[PathBuf]) -> Option<PathBuf> {
    let on_path = path_var.map(std::env::split_paths).into_iter().flatten();
    first_in("go", on_path.chain(homes.iter().cloned()))
}

/// The folder of the `go` [`go`] finds, when croft's own `PATH` does not
/// reach it: a program croft starts that runs `go` itself, gopls loading
/// packages or delve building the program, gets it on its `PATH` (#2807).
pub(crate) fn go_dir_off_path() -> Option<PathBuf> {
    go_dir_off_path_in(
        std::env::var_os("PATH").as_deref(),
        &GO_HOMES.map(PathBuf::from),
    )
}

/// [`go_dir_off_path`] on the `PATH` value `path_var`, with `homes`.
pub(crate) fn go_dir_off_path_in(path_var: Option<&OsStr>, homes: &[PathBuf]) -> Option<PathBuf> {
    if on_path_in("go", path_var).is_some() {
        return None;
    }
    go_in(path_var, homes)?.parent().map(Path::to_path_buf)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn file(path: &Path, mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// #2808: what a shell would run; a file without an execute bit, a
    /// folder, a dangling link and nothing at all are no program.
    #[test]
    fn only_a_file_that_can_run_is_a_program() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = tmp.path().join("tool");
        file(&tool, 0o755);
        assert!(runnable(&tool));
        let plain = tmp.path().join("plain");
        file(&plain, 0o644);
        assert!(!runnable(&plain), "no execute bit");
        let folder = tmp.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!runnable(&folder), "a folder");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&tool, &link).unwrap();
        assert!(runnable(&link), "a link to a program");
        let dangling = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("gone"), &dangling).unwrap();
        assert!(!runnable(&dangling), "a link to nothing");
        assert!(!runnable(&tmp.path().join("missing")));
    }

    /// #2808: a PATH scan passes over a file it cannot run for the next
    /// folder's program, as a shell does.
    #[test]
    fn a_path_scan_passes_over_what_cannot_run() {
        let tmp = tempfile::tempdir().unwrap();
        let (first, second) = (tmp.path().join("a"), tmp.path().join("b"));
        file(&first.join("cargo"), 0o644);
        file(&second.join("cargo"), 0o755);
        let path_var = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            on_path_in("cargo", Some(&path_var)),
            Some(second.join("cargo"))
        );
        assert_eq!(on_path_in("cargo", None), None);
        assert_eq!(first_in("cargo", [&first]), None);
    }

    /// #2807: Go is found on PATH first, then where its installers put it,
    /// so a launcher's PATH without the official installer's folder still
    /// finds the Go installed there.
    #[test]
    fn go_is_found_on_path_then_where_its_installers_put_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (on_path, official) = (tmp.path().join("bin"), tmp.path().join("go/bin"));
        file(&official.join("go"), 0o755);
        let path_var = std::env::join_paths([&on_path]).unwrap();
        let homes = [official.clone()];
        assert_eq!(go_in(Some(&path_var), &homes), Some(official.join("go")));
        assert_eq!(go_in(None, &homes), Some(official.join("go")));
        file(&on_path.join("go"), 0o755);
        assert_eq!(go_in(Some(&path_var), &homes), Some(on_path.join("go")));
        assert_eq!(go_in(None, &[]), None);
    }

    /// #2807 review: a Go found off croft's `PATH` is put on the `PATH` of
    /// gopls and delve, which run `go` themselves; one on croft's `PATH`
    /// needs nothing added.
    #[test]
    fn go_off_path_is_named_for_the_programs_that_run_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (on_path, official) = (tmp.path().join("bin"), tmp.path().join("go/bin"));
        std::fs::create_dir_all(&on_path).unwrap();
        file(&official.join("go"), 0o755);
        let path_var = std::env::join_paths([&on_path]).unwrap();
        let homes = [official.clone()];
        assert_eq!(go_dir_off_path_in(Some(&path_var), &homes), Some(official));
        file(&on_path.join("go"), 0o755);
        assert_eq!(go_dir_off_path_in(Some(&path_var), &homes), None);
        assert_eq!(go_dir_off_path_in(None, &[]), None);
    }
}
