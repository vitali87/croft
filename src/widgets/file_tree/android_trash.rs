//! Moving an entry to the trash on Android, where the `trash` crate has none: the
//! freedesktop.org home trash, moved out of file_tree.rs (#2062).

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn trash_one(path: &Path) -> io::Result<()> {
    let trash = trash_dir()?;
    let files = trash.join("files");
    let info = trash.join("info");
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;

    let base = path
        .file_name()
        .ok_or_else(|| io::Error::other("path has no file name"))?;
    let name = unique_name(&files, &info, base);

    // Spec: write the .trashinfo entry before moving the file so a reader
    // never sees a trashed file whose origin has not been recorded yet.
    let info_path = info.join(format!("{name}.trashinfo"));
    let origin = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut entry = fs::File::create(&info_path)?;
    write!(
        entry,
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(origin.as_os_str().as_encoded_bytes()),
        deletion_date()
    )?;

    let dest = files.join(&name);
    if let Err(e) = move_entry(path, &dest) {
        // The move failed: drop the info record so we never leak a
        // .trashinfo pointing at a file that was never trashed.
        let _ = fs::remove_file(&info_path);
        return Err(e);
    }
    Ok(())
}

fn trash_dir() -> io::Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("Trash"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/Trash"))
}

/// Pick a name unused in both `files/` and `info/`, suffixing `.1`, `.2`,
/// ... on collision so two same-named deletions never clobber each other.
fn unique_name(files: &Path, info: &Path, base: &OsStr) -> String {
    let base = base.to_string_lossy();
    let taken =
        |name: &str| files.join(name).exists() || info.join(format!("{name}.trashinfo")).exists();
    if !taken(&base) {
        return base.into_owned();
    }
    let mut n = 1u64;
    loop {
        let candidate = format!("{base}.{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Percent-encode per the spec (RFC 2396): keep unreserved bytes and the
/// `/` separators literal, escape everything else as `%XX`.
fn percent_encode(bytes: &[u8]) -> String {
    const UNRESERVED: &[u8] = b"-_.!~*'()";
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || UNRESERVED.contains(&b) || b == b'/' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex(b >> 4));
            out.push(hex(b & 0x0f));
        }
    }
    out
}

fn hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Local-time `YYYY-MM-DDThh:mm:ss` via libc, already a croft dependency,
/// so no date crate is pulled in for one android-only field.
fn deletion_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Rename when possible; fall back to recursive copy+remove across a
/// filesystem boundary (EXDEV), e.g. trashing from /sdcard into $HOME.
fn move_entry(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_recursive(src, dst)?;
            remove_recursive(src)
        }
        Err(e) => Err(e),
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if fs::symlink_metadata(src)?.is_dir() {
        fs::create_dir_all(dst)?;
        for child in fs::read_dir(src)? {
            let child = child?;
            copy_recursive(&child.path(), &dst.join(child.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn remove_recursive(src: &Path) -> io::Result<()> {
    if fs::symlink_metadata(src)?.is_dir() {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    }
}
