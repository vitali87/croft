//! `path:line` and `path:line:col` on the command line (#1487): the form
//! compilers, linters, `grep -n` and stack traces print, which `code -g`,
//! Zed and Helix all open at that spot.
//!
//! The suffix is split off only when the whole argument does not exist and
//! the path before it does, so a file whose real name ends in `:5` still
//! opens as itself.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A 1-based line, and optionally a 1-based column, to put the caret on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLocation {
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
}

/// `arg` as typed, with a trailing `:line` or `:line:col` split off when
/// `arg` does not name anything under `cwd` but the part before the suffix
/// does. A trailing `:` (as `grep -n` leaves it) is allowed.
pub fn split(arg: &Path, cwd: &Path) -> (PathBuf, Option<FileLocation>) {
    let exists = |p: &Path| cwd.join(p).exists();
    if exists(arg) {
        return (arg.to_path_buf(), None);
    }
    let bytes = arg.as_os_str().as_bytes();
    let bytes = bytes.strip_suffix(b":").unwrap_or(bytes);
    let Some((rest, last)) = split_number(bytes) else {
        return (arg.to_path_buf(), None);
    };
    // `file:line:col` first, then `file:line`.
    if let Some((prefix, line)) = split_number(rest) {
        let path = Path::new(OsStr::from_bytes(prefix));
        if !prefix.is_empty() && exists(path) {
            return (
                path.to_path_buf(),
                Some(FileLocation {
                    line: line.max(1),
                    col: Some(last.max(1)),
                }),
            );
        }
    }
    let path = Path::new(OsStr::from_bytes(rest));
    if !rest.is_empty() && exists(path) {
        return (
            path.to_path_buf(),
            Some(FileLocation {
                line: last.max(1),
                col: None,
            }),
        );
    }
    (arg.to_path_buf(), None)
}

/// `b"x:12"` -> `(b"x", 12)`: the digits after the last `:`.
fn split_number(bytes: &[u8]) -> Option<(&[u8], u32)> {
    let colon = bytes.iter().rposition(|&b| b == b':')?;
    let digits = &bytes[colon + 1..];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((&bytes[..colon], n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(tmp.path().join(f), "x\n").unwrap();
        }
        tmp
    }

    #[test]
    fn line_and_column_are_split_off_an_existing_file() {
        let tmp = dir_with(&["four.c"]);
        assert_eq!(
            split(Path::new("four.c:5:5"), tmp.path()),
            (
                PathBuf::from("four.c"),
                Some(FileLocation {
                    line: 5,
                    col: Some(5)
                })
            )
        );
        assert_eq!(
            split(Path::new("four.c:12"), tmp.path()),
            (
                PathBuf::from("four.c"),
                Some(FileLocation {
                    line: 12,
                    col: None
                })
            )
        );
        // `grep -n` style, with the colon before the matched text.
        assert_eq!(
            split(Path::new("four.c:7:"), tmp.path()).1,
            Some(FileLocation { line: 7, col: None })
        );
    }

    #[test]
    fn an_absolute_path_with_a_location_splits_too() {
        let tmp = dir_with(&["a.rs"]);
        let arg = format!("{}:3:9", tmp.path().join("a.rs").display());
        assert_eq!(
            split(Path::new(&arg), Path::new("/")),
            (
                tmp.path().join("a.rs"),
                Some(FileLocation {
                    line: 3,
                    col: Some(9)
                })
            )
        );
    }

    /// Negative: a file whose real name ends in `:5` opens as itself.
    #[test]
    fn a_file_that_really_has_the_suffix_is_left_alone() {
        let tmp = dir_with(&["notes:5", "notes"]);
        assert_eq!(
            split(Path::new("notes:5"), tmp.path()),
            (PathBuf::from("notes:5"), None)
        );
    }

    /// Negative: nothing is split when the file before the suffix is missing
    /// too, or the suffix is not numbers.
    #[test]
    fn a_missing_file_or_a_non_numeric_suffix_is_not_split() {
        let tmp = dir_with(&["four.c"]);
        for arg in ["gone.c:5:5", "four.c:x", "four.c:5:x", ":5", "four.c:"] {
            assert_eq!(
                split(Path::new(arg), tmp.path()),
                (PathBuf::from(arg), None),
                "{arg}"
            );
        }
    }

    #[test]
    fn line_zero_means_the_first_line() {
        let tmp = dir_with(&["four.c"]);
        assert_eq!(
            split(Path::new("four.c:0:0"), tmp.path()).1,
            Some(FileLocation {
                line: 1,
                col: Some(1)
            })
        );
    }
}
