//! Archive browser core (#179): list the members of a zip / jar / whl /
//! tar / tar.gz archive without touching payloads, and extract ONE
//! member on demand into a scratch directory so the standard open
//! dispatch (text, image, PDF, sheet, hex) can render it. Extraction is
//! containment-checked: a member whose path escapes the target
//! directory (zip-slip) is refused.

use std::path::{Path, PathBuf};

/// Per-member extraction cap: a member is opened through the normal
/// in-memory/editor paths, so an unbounded entry could balloon.
pub const MEMBER_CAP: u64 = 100 * 1024 * 1024;

/// Listing cap for tar-family archives (#198 review): walking tar
/// headers must read THROUGH every payload (and a GzDecoder is not
/// seekable), so listing cost is proportional to the archive - unlike
/// zip's central directory. Larger files fall to the hex viewer.
pub const TAR_LIST_CAP: u64 = 100 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

impl ArchiveKind {
    pub fn label(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "ZIP",
            ArchiveKind::Tar => "TAR",
            ArchiveKind::TarGz => "TAR.GZ",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Member path as stored (forward slashes).
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveView {
    pub kind: ArchiveKind,
    pub source_byte_size: u64,
    pub entries: Vec<ArchiveEntry>,
    pub selected: usize,
    pub scroll: usize,
    /// Frame truth for mouse hit-testing: first visible row's screen y
    /// and the painted row count.
    pub rows_top: u16,
    pub rows_visible: u16,
}

pub fn kind_from_ext(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Some(ArchiveKind::TarGz);
    }
    if name.ends_with(".tar") {
        return Some(ArchiveKind::Tar);
    }
    if name.ends_with(".zip") || name.ends_with(".jar") || name.ends_with(".whl") {
        return Some(ArchiveKind::Zip);
    }
    None
}

/// List members without reading payloads: the zip central directory, or
/// one pass over tar headers (gz inflates header-by-header, still no
/// payload retention).
pub fn list(path: &Path, kind: ArchiveKind) -> std::io::Result<ArchiveView> {
    let meta = std::fs::metadata(path)?;
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    match kind {
        ArchiveKind::Zip => {
            let f = std::fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
            for i in 0..z.len() {
                let e = z.by_index_raw(i).map_err(std::io::Error::other)?;
                entries.push(ArchiveEntry {
                    path: e.name().to_string(),
                    size: e.size(),
                    dir: e.is_dir(),
                });
            }
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            if meta.len() > TAR_LIST_CAP {
                return Err(std::io::Error::other(format!(
                    "tar archive too large to list ({} bytes)",
                    meta.len()
                )));
            }
            let f = std::fs::File::open(path)?;
            let read: Box<dyn std::io::Read> = if kind == ArchiveKind::TarGz {
                Box::new(flate2::read::GzDecoder::new(f))
            } else {
                Box::new(f)
            };
            let mut a = tar::Archive::new(read);
            for entry in a.entries()? {
                let e = entry?;
                let hdr = e.header();
                entries.push(ArchiveEntry {
                    path: e.path()?.to_string_lossy().into_owned(),
                    size: hdr.size().unwrap_or(0),
                    dir: hdr.entry_type().is_dir(),
                });
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ArchiveView {
        kind,
        source_byte_size: meta.len(),
        entries,
        selected: 0,
        scroll: 0,
        rows_top: 0,
        rows_visible: 0,
    })
}

/// Extract one member into `dest_dir` (created on demand), returning
/// the extracted file's path. Containment-checked: the joined path must
/// stay inside `dest_dir` after component normalisation, so `../` and
/// absolute member names are refused, never written.
pub fn extract_member(
    path: &Path,
    kind: ArchiveKind,
    member: &str,
    dest_dir: &Path,
) -> std::io::Result<PathBuf> {
    let dest = contained_join(dest_dir, member)
        .ok_or_else(|| std::io::Error::other(format!("refusing unsafe member path: {member}")))?;
    match kind {
        ArchiveKind::Zip => {
            let f = std::fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
            let mut e = z.by_name(member).map_err(std::io::Error::other)?;
            if e.size() > MEMBER_CAP {
                return Err(std::io::Error::other(format!(
                    "member too large ({} bytes)",
                    e.size()
                )));
            }
            write_out(&dest, &mut e)?;
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let f = std::fs::File::open(path)?;
            let read: Box<dyn std::io::Read> = if kind == ArchiveKind::TarGz {
                Box::new(flate2::read::GzDecoder::new(f))
            } else {
                Box::new(f)
            };
            let mut a = tar::Archive::new(read);
            for entry in a.entries()? {
                let mut e = entry?;
                if e.path()?.to_string_lossy() == member {
                    if e.header().size().unwrap_or(0) > MEMBER_CAP {
                        return Err(std::io::Error::other("member too large"));
                    }
                    write_out(&dest, &mut e)?;
                    return Ok(dest);
                }
            }
            return Err(std::io::Error::other(format!("no such member: {member}")));
        }
    }
    Ok(dest)
}

fn write_out(dest: &Path, read: &mut dyn std::io::Read) -> std::io::Result<()> {
    write_out_capped(dest, read, MEMBER_CAP)
}

/// Copy at most `cap` bytes; a stream that keeps going past it (a
/// member whose DECLARED size lied - deflate does not bound its output
/// to the metadata, #198 review) fails and the partial file is removed.
fn write_out_capped(dest: &Path, read: &mut dyn std::io::Read, cap: u64) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = std::fs::File::create(dest)?;
    let mut limited = std::io::Read::take(read, cap + 1);
    let copied = std::io::copy(&mut limited, &mut out)?;
    if copied > cap {
        drop(out);
        let _ = std::fs::remove_file(dest);
        return Err(std::io::Error::other(
            "member stream exceeded the size cap; partial output removed",
        ));
    }
    Ok(())
}

/// Join `member` under `dir`, refusing absolute paths, drive prefixes,
/// and any `..` traversal. Pure lexical containment: nothing is written
/// while deciding.
pub fn contained_join(dir: &Path, member: &str) -> Option<PathBuf> {
    use std::path::Component;
    let rel = Path::new(member);
    let mut out = dir.to_path_buf();
    let mut depth = 0usize;
    for c in rel.components() {
        match c {
            Component::Normal(seg) => {
                out.push(seg);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (depth > 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn make_zip(p: &Path) {
        let f = std::fs::File::create(p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.add_directory("docs/", opts).unwrap();
        z.start_file("docs/readme.txt", opts).unwrap();
        z.write_all(b"hello from zip").unwrap();
        z.start_file("evil.txt", opts).unwrap();
        z.write_all(b"ok").unwrap();
        z.finish().unwrap();
    }

    fn make_tar_gz(p: &Path) {
        let f = std::fs::File::create(p).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        let data = b"tar payload";
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        t.append_data(&mut hdr, "inner/file.bin", &data[..])
            .unwrap();
        t.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn lists_zip_and_targz_members_sorted_without_payload_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("a.zip");
        make_zip(&zp);
        let v = list(&zp, ArchiveKind::Zip).unwrap();
        let names: Vec<&str> = v.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["docs/", "docs/readme.txt", "evil.txt"]);
        assert!(v.entries[0].dir);
        assert_eq!(v.entries[1].size, 14);

        let tp = tmp.path().join("a.tar.gz");
        make_tar_gz(&tp);
        let v = list(&tp, ArchiveKind::TarGz).unwrap();
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].path, "inner/file.bin");
        assert_eq!(v.entries[0].size, 11);
    }

    #[test]
    fn extracts_a_member_and_refuses_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("a.zip");
        make_zip(&zp);
        let dest = tmp.path().join("out");
        let got = extract_member(&zp, ArchiveKind::Zip, "docs/readme.txt", &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "hello from zip");
        assert!(got.starts_with(&dest));

        let tp = tmp.path().join("a.tar.gz");
        make_tar_gz(&tp);
        let got = extract_member(&tp, ArchiveKind::TarGz, "inner/file.bin", &dest).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"tar payload");

        assert!(extract_member(&zp, ArchiveKind::Zip, "../escape.txt", &dest).is_err());
        assert!(extract_member(&zp, ArchiveKind::Zip, "/abs.txt", &dest).is_err());
        assert!(!dest.join("..").join("escape.txt").exists());
    }

    #[test]
    fn contained_join_is_strictly_lexical() {
        let d = Path::new("/safe/dir");
        assert_eq!(
            contained_join(d, "a/b.txt").unwrap(),
            Path::new("/safe/dir/a/b.txt")
        );
        assert_eq!(
            contained_join(d, "a/../b.txt").unwrap(),
            Path::new("/safe/dir/b.txt")
        );
        assert!(contained_join(d, "../evil").is_none());
        assert!(contained_join(d, "a/../../evil").is_none());
        assert!(contained_join(d, "/etc/passwd").is_none());
        assert!(contained_join(d, ".").is_none(), "no file at all");
    }

    #[test]
    fn write_out_caps_the_actual_stream_not_the_declared_size() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("x.bin");
        let mut endless = std::io::repeat(7u8);
        let err = write_out_capped(&dest, &mut endless, 1024).unwrap_err();
        assert!(err.to_string().contains("size cap"));
        assert!(!dest.exists(), "partial output removed");
        let mut small = &b"ok"[..];
        write_out_capped(&dest, &mut small, 1024).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ok");
    }

    #[test]
    fn oversized_tar_refuses_to_list() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.tar");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(TAR_LIST_CAP + 1).unwrap();
        drop(f);
        let err = list(&p, ArchiveKind::Tar).unwrap_err();
        assert!(err.to_string().contains("too large to list"));
    }

    #[test]
    fn kind_detection_covers_compound_extensions() {
        assert_eq!(
            kind_from_ext(Path::new("a.tar.gz")),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(kind_from_ext(Path::new("a.tgz")), Some(ArchiveKind::TarGz));
        assert_eq!(kind_from_ext(Path::new("a.tar")), Some(ArchiveKind::Tar));
        assert_eq!(kind_from_ext(Path::new("a.zip")), Some(ArchiveKind::Zip));
        assert_eq!(kind_from_ext(Path::new("lib.jar")), Some(ArchiveKind::Zip));
        assert_eq!(kind_from_ext(Path::new("pkg.whl")), Some(ArchiveKind::Zip));
        assert_eq!(kind_from_ext(Path::new("a.txt")), None);
    }
}
