//! Archive browser core (#179): list the members of a zip / jar / whl /
//! tar / tar.gz / tar.xz archive without touching payloads, and extract ONE
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

/// The largest zip this will LIST, bounded by file size because that is
/// the only measure available before the cost is paid (#493).
///
/// A zip's entry count is not bounded above by its file size, so the
/// editor's 50 MB cap does not bound the listing work: measured at 87
/// bytes per member, a 33 MB file declares 400k members and takes 1.65s
/// to list, which is a frozen editor on the frame loop.
///
/// It is bounded BELOW by file size, though, and that is what makes this
/// gate work. `ZipArchive::new` parses the whole central directory before
/// returning — allocating a name, extra field, comment and entry record
/// per member — so `len()` only exists once the expensive work is done.
/// Measured, that constructor is ~71% of the total (1.17s of the 1.65s at
/// 400k), so a guard on `len()` would arrive too late to prevent most of
/// what it is for. Gating the file size is the only bound that precedes
/// the parse, which is also what the tar branch does (#198).
///
/// 8 MB admits ~97k members at the measured 87 bytes each. The true
/// worst case is higher: `ZipArchive::new` reads only the CENTRAL
/// directory, whose entry is 46 bytes plus a 1-byte name, so a crafted
/// archive puts ~178k entries through the constructor, ~800ms. Bounded,
/// and still an order off the 400k that motivated this. For scale, a large
/// library jar measured here holds 38k members in 3.2 MB, so ordinary
/// archives pass with room; a zipped monorepo `node_modules` may not, and
/// is expected to fall through to the hex viewer with the reason shown.
pub const ZIP_LIST_CAP: u64 = 8 * 1024 * 1024;

/// Whether a listing error is a deliberate SIZE refusal rather than a
/// parse failure (#493). Owned here, beside the messages it matches, so a
/// reworded message cannot silently turn a caller's policy branch into its
/// ignore branch — which would restore the silent hex dump this exists to
/// explain. Covers every cap in this module: both listing gates and the
/// decoded-size cap a `.tar.gz` can trip while streaming.
pub fn is_list_cap_refusal(rendered: &str) -> bool {
    rendered.contains("too large to list") || rendered.contains("past the listing cap")
}

/// A reader that fails once more than `remaining` bytes have been
/// pulled through it (#198 review): the compressed size of a tar.gz
/// says nothing about the DECODED bytes a listing walk must read.
struct CappedRead<R> {
    inner: R,
    remaining: u64,
}

impl<R: std::io::Read> std::io::Read for CappedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(std::io::Error::other(
                "archive expands past the listing cap",
            ));
        }
        let take = buf
            .len()
            .min(self.remaining.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..take])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    /// `.tar.xz` / `.txz` (#1645): kernel, GNU, Python and Node source
    /// releases. Decoded with `lzma-rs`, already a dependency for the
    /// language-server installs.
    TarXz,
}

impl ArchiveKind {
    pub fn label(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "ZIP",
            ArchiveKind::Tar => "TAR",
            ArchiveKind::TarGz => "TAR.GZ",
            ArchiveKind::TarXz => "TAR.XZ",
        }
    }
}

/// The tar a `.tar.xz` holds, decoded whole: `lzma-rs` has no streaming xz
/// reader. So the listing cap is checked against the size the stream's own
/// index declares before a byte is decoded (an unreadable index is refused,
/// as the installer refuses it), and the output is capped again in case the
/// index lies. A decoded stream that is not a tar, a plain `notes.txt.xz`,
/// is an error (here or in the header walk), so the file falls through to
/// the hex viewer.
fn xz_tar_bytes(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let past_cap = |size: String| {
        std::io::Error::other(format!("archive expands past the listing cap ({size})"))
    };
    let bytes = std::fs::read(path)?;
    let declared = crate::lsp::install::xz_declared_size(&bytes)
        .ok_or_else(|| std::io::Error::other("the xz index cannot be read"))?;
    if declared > cap {
        return Err(past_cap(format!("{declared} bytes")));
    }
    let mut sink = crate::lsp::install::Capped {
        buf: Vec::new(),
        cap,
    };
    if let Err(e) = lzma_rs::xz_decompress(&mut std::io::Cursor::new(&bytes), &mut sink) {
        // The sink's refusal: the index understated the stream.
        let e = format!("{e:?}");
        return Err(if e.contains("ceiling") {
            past_cap(String::from("more than its index declares"))
        } else {
            std::io::Error::other(format!("xz: {e}"))
        });
    }
    // `tar::Archive` validates the headers (a checksum per member) as the
    // caller walks them, so a V7 tar with no `ustar` magic and an empty tar
    // with no blocks at all both list. A tar is whole 512-byte blocks, so
    // anything else, a plain `notes.txt.xz`, is refused here without a walk.
    let tar = sink.buf;
    if !tar.len().is_multiple_of(512) {
        return Err(std::io::Error::other("not a tar archive"));
    }
    Ok(tar)
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
    if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        return Some(ArchiveKind::TarXz);
    }
    if name.ends_with(".tar") {
        return Some(ArchiveKind::Tar);
    }
    // Zip containers by another name (#1645): Android and Java packages,
    // NuGet and VS Code extension packages, e-books. Content sniffing
    // already caught them; the extension makes the route explicit.
    const ZIP_EXTS: &[&str] = &[
        ".zip", ".jar", ".whl", ".apk", ".aar", ".war", ".ear", ".nupkg", ".vsix", ".epub",
    ];
    if ZIP_EXTS.iter().any(|ext| name.ends_with(ext)) {
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
            // Before the open, not after: the constructor below is where
            // the per-member cost is paid.
            if meta.len() > ZIP_LIST_CAP {
                return Err(std::io::Error::other(format!(
                    "zip too large to list ({} bytes)",
                    meta.len()
                )));
            }
            let f = std::fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
            for i in 0..z.len() {
                let e = z.by_index_raw(i).map_err(std::io::Error::other)?;
                entries.push(ArchiveEntry {
                    path: zip_member_name(&e),
                    size: e.size(),
                    dir: e.is_dir(),
                });
            }
        }
        ArchiveKind::Tar | ArchiveKind::TarGz | ArchiveKind::TarXz => {
            if meta.len() > TAR_LIST_CAP {
                return Err(std::io::Error::other(format!(
                    "tar archive too large to list ({} bytes)",
                    meta.len()
                )));
            }
            let f = std::fs::File::open(path)?;
            let read: Box<dyn std::io::Read> = match kind {
                // Cap the DECODED bytes too: a small .tar.gz can expand
                // far past the source-size gate above (#198 review).
                ArchiveKind::TarGz => Box::new(CappedRead {
                    inner: flate2::read::GzDecoder::new(f),
                    remaining: TAR_LIST_CAP,
                }),
                ArchiveKind::TarXz => {
                    Box::new(std::io::Cursor::new(xz_tar_bytes(path, TAR_LIST_CAP)?))
                }
                _ => Box::new(f),
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
    std::fs::create_dir_all(dest_dir)?;
    prepare_no_follow(dest_dir, &dest)?;
    match kind {
        ArchiveKind::Zip => {
            let f = std::fs::File::open(path)?;
            let mut z = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
            // By the name as listed, which can differ from the crate's own
            // reading of it (#1224).
            let index = (0..z.len())
                .find(|&i| {
                    z.by_index_raw(i)
                        .is_ok_and(|e| zip_member_name(&e) == member)
                })
                .ok_or_else(|| std::io::Error::other(format!("no such member: {member}")))?;
            let mut e = z.by_index(index).map_err(std::io::Error::other)?;
            if e.size() > MEMBER_CAP {
                return Err(std::io::Error::other(format!(
                    "member too large ({} bytes)",
                    e.size()
                )));
            }
            write_out(&dest, &mut e)?;
        }
        ArchiveKind::Tar | ArchiveKind::TarGz | ArchiveKind::TarXz => {
            let f = std::fs::File::open(path)?;
            let read: Box<dyn std::io::Read> = match kind {
                ArchiveKind::TarGz => Box::new(flate2::read::GzDecoder::new(f)),
                ArchiveKind::TarXz => {
                    Box::new(std::io::Cursor::new(xz_tar_bytes(path, TAR_LIST_CAP)?))
                }
                _ => Box::new(f),
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

/// A zip member's name as `unzip`, libarchive and 7-Zip read it (#1224):
/// UTF-8 when the raw bytes are UTF-8, even without general-purpose flag
/// bit 11, which Info-ZIP `zip` 3.0 never sets; CP437 (the crate's reading)
/// only for bytes that aren't. CP437 names with high bytes are almost never
/// valid UTF-8, so a real DOS archive still reads as CP437.
/// An ASCII raw name is left to the crate, which prefers a Unicode Path
/// extra field when an archiver stored one beside a placeholder name.
fn zip_member_name<R: std::io::Read>(e: &zip::read::ZipFile<'_, R>) -> String {
    let raw = e.name_raw();
    match std::str::from_utf8(raw) {
        Ok(name) if !raw.is_ascii() => name.to_string(),
        _ => e.name().to_string(),
    }
}

fn write_out(dest: &Path, read: &mut dyn std::io::Read) -> std::io::Result<()> {
    write_out_capped(dest, read, MEMBER_CAP)
}

/// Create the intermediate directories of `dest` below `root`,
/// REFUSING any component that exists as a symlink (#198 review:
/// create_dir_all + File::create follow existing links, so a planted
/// `dest/nested -> /elsewhere` redirected the write outside the root).
/// An existing symlink leaf is refused too; a regular file there is only
/// replaced once the new content is complete.
fn prepare_no_follow(root: &Path, dest: &Path) -> std::io::Result<()> {
    let rel = dest.strip_prefix(root).map_err(std::io::Error::other)?;
    let mut cur = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for c in &parts[..parts.len().saturating_sub(1)] {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(std::io::Error::other(format!(
                    "refusing to traverse a symlink inside the extraction root: {}",
                    cur.display()
                )));
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(std::io::Error::other(format!(
                    "a member path component is a file: {}",
                    cur.display()
                )));
            }
            Err(_) => std::fs::create_dir(&cur)?,
        }
    }
    match std::fs::symlink_metadata(dest) {
        Ok(m) if m.file_type().is_symlink() => Err(std::io::Error::other(
            "refusing to overwrite a symlink at the extraction target",
        )),
        // An existing file is kept until the member has fully extracted:
        // `write_out_capped` renames over it only on success.
        Ok(m) if m.is_file() => Ok(()),
        Ok(_) => Err(std::io::Error::other("extraction target is a directory")),
        Err(_) => Ok(()),
    }
}

/// Copy at most `cap` bytes; a stream that keeps going past it (a
/// member whose DECLARED size lied - deflate does not bound its output
/// to the metadata, #198 review) fails and the partial file is removed.
///
/// Written to a fresh temp file beside `dest` and renamed over it only once
/// complete: a member that fails midway (a bad checksum, an unsupported
/// method, the cap) used to cost the user the file already at `dest`, which
/// was unlinked up front, and leave a partial file in its place.
fn write_out_capped(dest: &Path, read: &mut dyn std::io::Read, cap: u64) -> std::io::Result<()> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dest.with_file_name(format!(".{name}.croft-extract-{}", std::process::id()));
    let result = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let mut limited = std::io::Read::take(read, cap + 1);
        let copied = std::io::copy(&mut limited, &mut out)?;
        if copied > cap {
            return Err(std::io::Error::other(
                "member stream exceeded the size cap; partial output removed",
            ));
        }
        std::fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

    /// A zip large enough to cost real time lists nothing rather than
    /// parking the frame loop (#493). The gate is on the file, before the
    /// parse: `ZipArchive::new` builds a record per member, so a bound read
    /// off the parsed archive would arrive after most of the cost.
    #[test]
    fn a_zip_over_the_list_cap_refuses_to_list() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.zip");
        // 80, not the measured 87 bytes a member, for margin: this must
        // clear the cap without writing a pointlessly enormous fixture.
        write_zip_with_entries(&p, (ZIP_LIST_CAP / 80) as usize);
        let size = std::fs::metadata(&p).unwrap().len();
        assert!(size > ZIP_LIST_CAP, "fixture: {size} bytes clears the cap");
        assert!(
            size < 50 * 1024 * 1024,
            "and stays under the editor's general size cap, so only this \
             guard could have caught it"
        );
        let err = list(&p, ArchiveKind::Zip).unwrap_err();
        assert!(
            err.to_string().contains("too large to list"),
            "says why: {err}"
        );
    }

    /// A refused listing still opens, as hex, AND says why (#493): the cap
    /// bounds the work, not the user's look at the file, and a browser
    /// silently replaced by a hex dump reads as a bug rather than a policy.
    #[test]
    fn a_zip_over_the_list_cap_opens_as_hex_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.zip");
        write_zip_with_entries(&p, (ZIP_LIST_CAP / 80) as usize);
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(
            e.hex.is_some(),
            "the file is still viewable, just not as an archive"
        );
        assert!(e.archive.is_none(), "and not as the archive it refused");
        assert!(
            e.status.contains("too large to list"),
            "and the status says why it is hex: {}",
            e.status
        );
    }

    /// The gate is `>`, so a file exactly at the cap still lists (#493
    /// review): an off-by-one to `>=` would refuse a legitimate archive
    /// and no other test straddles the edge. Sparse and not a real zip, so
    /// it must fail the PARSE rather than the size gate.
    #[test]
    fn a_zip_exactly_at_the_list_cap_is_not_refused_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("atcap.zip");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(ZIP_LIST_CAP).unwrap();
        drop(f);
        let err = list(&p, ArchiveKind::Zip).unwrap_err();
        assert!(
            !err.to_string().contains("too large to list"),
            "at the cap the size gate must not fire, leaving the parse to \
             reject it: {err}"
        );
    }

    /// A tar.gz refused by its DECODED size explains itself through the
    /// magic route too (#493 review): the zip branch was fixed for that
    /// asymmetry, and an extensionless archive reaches `open_archive` the
    /// same second way. A compressed bomb is the reachable case on THIS
    /// route: an extensionless tar over the 100MB raw cap is binary-headed,
    /// so the editor's 50MB guard sends it to hex before magic sniffing
    /// runs. The raw cap is still live BY EXTENSION, where `kind_from_ext`
    /// runs above that guard, and the test below covers it.
    #[test]
    fn an_extensionless_targz_past_the_decoded_cap_says_why_in_the_hex_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("noext");
        {
            let f = std::fs::File::create(&p).unwrap();
            let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut t = tar::Builder::new(gz);
            let zeros = vec![0u8; (TAR_LIST_CAP / 4) as usize];
            for i in 0..5 {
                let mut hdr = tar::Header::new_gnu();
                hdr.set_size(zeros.len() as u64);
                hdr.set_cksum();
                t.append_data(&mut hdr, format!("z{i}.bin"), &zeros[..])
                    .unwrap();
            }
            t.finish().unwrap();
        }
        assert!(
            std::fs::metadata(&p).unwrap().len() < 50 * 1024 * 1024,
            "fixture: under the editor's general cap, so magic routing runs"
        );
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(e.hex.is_some(), "still viewable, just not as an archive");
        assert!(
            crate::archive::is_list_cap_refusal(&e.status),
            "and the reason reaches the reader on this route too: {}",
            e.status
        );
    }

    /// The raw tar cap is live BY EXTENSION (#493 review): `kind_from_ext`
    /// routes above the editor's own 50MB guard, so a big `.tar` reaches
    /// the archive listing and its refusal must reach the reader too. The
    /// magic route cannot get here, which is what the sibling test covers.
    #[test]
    fn an_oversized_raw_tar_says_why_in_the_hex_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.tar");
        {
            // A real ustar header, so the head sniffs binary and the size
            // guard routes to hex rather than bailing; sparse past the cap.
            let mut header = vec![0u8; 512];
            header[..5].copy_from_slice(b"a.bin");
            header[257..262].copy_from_slice(b"ustar");
            std::fs::write(&p, &header).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_len(TAR_LIST_CAP + 1).unwrap();
        }
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(e.hex.is_some(), "still viewable, just not as an archive");
        assert!(
            crate::archive::is_list_cap_refusal(&e.status),
            "the raw cap's refusal reaches the reader: {}",
            e.status
        );
    }

    /// A note must not outlive the condition that armed it (#493 review).
    /// Keying stops a note explaining a DIFFERENT file; it does not stop one
    /// going stale against its own. `open_hex`'s same-path refresh returns
    /// without consuming the note, so a reload of an over-cap archive leaves
    /// one armed, and if the file then comes under the cap that reason must
    /// not explain the next open.
    #[test]
    fn a_stale_refusal_reason_does_not_explain_the_same_file_later() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shrinking.zip");
        write_zip_with_entries(&p, (ZIP_LIST_CAP / 80) as usize);
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(
            e.hex.is_some() && crate::archive::is_list_cap_refusal(&e.status),
            "staging: over the cap, hex with the reason: {}",
            e.status
        );
        // A same-path reload: the note is re-armed and the refresh returns
        // without consuming it.
        e.open(&p).unwrap();
        // The file now lists fine.
        write_zip_with_entries(&p, 10);
        e.open(&p).unwrap();
        assert!(e.archive.is_some(), "staging: it lists now");
        // "Reopen as Hex" on the same path.
        e.open_hex(&p).unwrap();
        assert!(
            !crate::archive::is_list_cap_refusal(&e.status),
            "a reason from when it was over the cap must not explain it \
             now that it is not: {}",
            e.status
        );
    }

    /// The reason describes only the file it was recorded for (#493
    /// review). An open can arm the note and then fail before reaching any
    /// viewer, and "Reopen as Hex" reaches `open_hex` without passing
    /// through `open` at all, so a note is not guaranteed to be consumed
    /// by the open that set it. Keyed on the path, a survivor explains
    /// nothing rather than explaining the wrong file.
    ///
    /// The failing open here is deterministic, not a race: a `.zip` over
    /// the editor's own 50MB cap whose head is TEXT arms the note at the
    /// archive gate and then bails at the size guard, reaching no viewer.
    #[test]
    fn a_refusal_reason_does_not_explain_a_later_unrelated_file() {
        let dir = tempfile::tempdir().unwrap();
        let armed = dir.path().join("armed.zip");
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&armed).unwrap();
            // The whole 4096-byte head must be text: the sniff reads that
            // much, and a sparse tail would put NUL bytes in it and route
            // to hex instead of bailing. Sparse only BEYOND the head, so
            // the fixture still costs nothing.
            f.write_all(&b"plain text head, not binary\n".repeat(200))
                .unwrap();
            f.set_len(60 * 1024 * 1024).unwrap();
        }
        let mut e = crate::widgets::editor::Editor::new();
        assert!(
            e.open(&armed).is_err(),
            "precondition: this open must reach no viewer, so the note it \
             armed is left behind rather than consumed"
        );

        // An unrelated file, viewed as hex the way "Reopen as Hex" does.
        let plain = dir.path().join("plain.bin");
        std::fs::write(&plain, [0u8, 1, 2, 3, 0, 255]).unwrap();
        e.open_hex(&plain).unwrap();
        assert!(
            !e.status.contains("too large to list"),
            "a note from another file must not explain this one: {}",
            e.status
        );
    }

    /// The same refusal on the magic-sniffed route (#493 review): an
    /// extensionless zip reaches `open_archive` by a second path, which
    /// must be bounded too.
    #[test]
    fn an_extensionless_zip_over_the_list_cap_also_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("noext");
        write_zip_with_entries(&p, (ZIP_LIST_CAP / 80) as usize);
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(e.hex.is_some(), "sniffed as a zip, refused, still viewable");
        assert!(e.archive.is_none());
    }

    /// An ordinary archive still lists, so the guard cannot be satisfied
    /// by refusing everything. Sized after a real library jar measured at
    /// 38k members in 3.2 MB, the kind of file that must keep working.
    #[test]
    fn an_ordinary_zip_under_the_list_cap_still_lists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ordinary.zip");
        write_zip_with_entries(&p, 38_000);
        let size = std::fs::metadata(&p).unwrap().len();
        assert!(
            size < ZIP_LIST_CAP,
            "fixture: {size} bytes is under the cap"
        );
        let view = list(&p, ArchiveKind::Zip).unwrap();
        assert_eq!(view.entries.len(), 38_000);
    }

    /// Build a zip with `n` empty stored members, through the crate the
    /// reader uses, so the fixture cannot disagree with it about the format.
    fn write_zip_with_entries(path: &std::path::Path, n: usize) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for i in 0..n {
            z.start_file(i.to_string(), o).unwrap();
        }
        z.finish().unwrap();
    }
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

    /// A zip of `name` holding `body`, written as Info-ZIP `zip` 3.0 does:
    /// general-purpose flag bit 11 (names are UTF-8) cleared in every
    /// header. `patch` replaces a placeholder byte of the name with a raw
    /// one, for names the writer can't take as a `&str`.
    fn info_zip_style(p: &Path, name: &str, body: &[u8], patch: Option<(u8, u8)>) {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file(name, o).unwrap();
        z.write_all(body).unwrap();
        z.finish().unwrap();
        let mut bytes = out.into_inner();
        for i in 0..bytes.len().saturating_sub(10) {
            let flags_at = match &bytes[i..i + 4] {
                b"PK\x03\x04" => i + 6,
                b"PK\x01\x02" => i + 8,
                _ => continue,
            };
            bytes[flags_at + 1] &= !0x08;
        }
        if let Some((from, to)) = patch {
            for b in bytes.iter_mut().filter(|b| **b == from) {
                *b = to;
            }
        }
        std::fs::write(p, bytes).unwrap();
    }

    /// #1224: Info-ZIP stores UTF-8 names without flag bit 11; they list
    /// and extract under their real names, not as CP437 mojibake.
    #[test]
    fn a_utf8_name_without_the_utf8_flag_lists_and_extracts_as_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("w.zip");
        let name = "src/ünï cödé/файл.txt";
        info_zip_style(&zp, name, b"x\n", None);
        let v = list(&zp, ArchiveKind::Zip).unwrap();
        assert_eq!(v.entries[0].path, name);
        let dest = tmp.path().join("out");
        let got = extract_member(&zp, ArchiveKind::Zip, name, &dest).unwrap();
        assert_eq!(got, dest.join(name));
        assert_eq!(std::fs::read(&got).unwrap(), b"x\n");
    }

    /// Negative (#1224): a name that isn't UTF-8 is still CP437, as the zip
    /// spec says for a clear bit 11, and extracts under that reading.
    #[test]
    fn a_cp437_name_still_reads_as_cp437() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("dos.zip");
        // 0x82 is CP437 `é`, and on its own no UTF-8 at all.
        info_zip_style(&zp, "caf~.txt", b"menu", Some((b'~', 0x82)));
        let v = list(&zp, ArchiveKind::Zip).unwrap();
        assert_eq!(v.entries[0].path, "café.txt");
        let dest = tmp.path().join("out");
        let got = extract_member(&zp, ArchiveKind::Zip, "café.txt", &dest).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"menu");
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
    fn a_failed_extraction_keeps_the_file_already_there() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf[0] = b'X';
                static CALLS: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                if CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    Ok(1)
                } else {
                    Err(std::io::Error::other("Invalid checksum"))
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("notes.txt");
        std::fs::write(&dest, "PRECIOUS").unwrap();
        prepare_no_follow(tmp.path(), &dest).unwrap();
        assert!(write_out_capped(&dest, &mut Broken, 1024).is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "PRECIOUS");
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no partial file left behind");
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
    fn targz_expanding_past_the_cap_is_refused_at_list() {
        // A tiny compressed file whose DECODED stream blows the cap.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("bomb.tar.gz");
        let f = std::fs::File::create(&p).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        let zeros = vec![0u8; (TAR_LIST_CAP / 4) as usize];
        for i in 0..5 {
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(zeros.len() as u64);
            hdr.set_cksum();
            t.append_data(&mut hdr, format!("z{i}.bin"), &zeros[..])
                .unwrap();
        }
        t.into_inner().unwrap().finish().unwrap();
        assert!(
            std::fs::metadata(&p).unwrap().len() < TAR_LIST_CAP,
            "the compressed file itself is small"
        );
        let err = list(&p, ArchiveKind::TarGz).unwrap_err();
        assert!(err.to_string().contains("listing cap"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn extraction_refuses_symlinked_components_and_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("a.zip");
        make_zip(&zp);
        let dest = tmp.path().join("out");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // dest/docs -> outside: the member docs/readme.txt must refuse.
        std::os::unix::fs::symlink(&outside, dest.join("docs")).unwrap();
        let err = extract_member(&zp, ArchiveKind::Zip, "docs/readme.txt", &dest).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!outside.join("readme.txt").exists(), "nothing escaped");
        // A symlink LEAF is refused too.
        std::fs::remove_file(dest.join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.join("evil.txt"), dest.join("evil.txt")).unwrap();
        let err = extract_member(&zp, ArchiveKind::Zip, "evil.txt", &dest).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!outside.join("evil.txt").exists());
    }

    /// #1645: the issue's `proj-1.0.tar.xz`, built from a tar of `proj/`,
    /// `proj/README.md` and `proj/main.rs`.
    fn make_tar_xz(p: &Path) {
        let mut t = tar::Builder::new(Vec::new());
        let mut dir = tar::Header::new_ustar();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        dir.set_mode(0o755);
        dir.set_cksum();
        t.append_data(&mut dir, "proj/", std::io::empty()).unwrap();
        for (name, body) in [
            ("proj/README.md", "hello\n"),
            ("proj/main.rs", "fn main(){}\n"),
        ] {
            let mut hdr = tar::Header::new_ustar();
            hdr.set_size(body.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            t.append_data(&mut hdr, name, body.as_bytes()).unwrap();
        }
        let tar_bytes = t.into_inner().unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(&tar_bytes), &mut xz).unwrap();
        std::fs::write(p, xz).unwrap();
    }

    /// #1645: a `.tar.xz`, a `.txz` and the same bytes with no extension all
    /// browse as `TAR.XZ` with their three members, where each opened as a
    /// hex dump.
    #[test]
    fn a_tar_xz_lists_its_members_by_extension_and_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["proj-1.0.tar.xz", "proj-1.0.txz", "release-asset"] {
            let p = tmp.path().join(name);
            make_tar_xz(&p);
            let mut e = crate::widgets::editor::Editor::new();
            e.open(&p).unwrap();
            let view = e
                .archive
                .as_ref()
                .unwrap_or_else(|| panic!("{name} opens as an archive, not hex"));
            assert_eq!(view.kind.label(), "TAR.XZ", "{name}");
            let names: Vec<&str> = view.entries.iter().map(|m| m.path.as_str()).collect();
            assert_eq!(
                names,
                vec!["proj/", "proj/README.md", "proj/main.rs"],
                "{name}"
            );
        }
    }

    /// #1645: a member of a `.tar.xz` extracts like one of a `.tar.gz`, so
    /// Enter on it opens the file.
    #[test]
    fn a_tar_xz_member_extracts() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("proj-1.0.tar.xz");
        make_tar_xz(&p);
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        let kind = e.archive.as_ref().expect("listed").kind;
        let out = extract_member(&p, kind, "proj/main.rs", &tmp.path().join("out")).unwrap();
        assert_eq!(std::fs::read_to_string(out).unwrap(), "fn main(){}\n");
    }

    /// #1645: a V7 tar (no `ustar` magic, `tar --format=v7`) and an empty
    /// tar (zero blocks) inside xz both list, where each fell through to hex.
    #[test]
    fn a_v7_or_empty_tar_xz_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = tar::Builder::new(Vec::new());
        let body = "old\n";
        let mut hdr = tar::Header::new_old();
        hdr.set_size(body.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        t.append_data(&mut hdr, "v7.txt", body.as_bytes()).unwrap();
        let v7 = t.into_inner().unwrap();
        assert_ne!(&v7[257..262], b"ustar");
        for (name, tar_bytes, want) in [
            ("v7.tar.xz", v7, vec!["v7.txt"]),
            ("empty.tar.xz", Vec::new(), vec![]),
        ] {
            let p = tmp.path().join(name);
            let mut xz = Vec::new();
            lzma_rs::xz_compress(&mut std::io::Cursor::new(&tar_bytes), &mut xz).unwrap();
            std::fs::write(&p, xz).unwrap();
            let mut e = crate::widgets::editor::Editor::new();
            e.open(&p).unwrap();
            let view = e
                .archive
                .as_ref()
                .unwrap_or_else(|| panic!("{name} opens as an archive, not hex"));
            assert_eq!(view.kind.label(), "TAR.XZ", "{name}");
            let names: Vec<&str> = view.entries.iter().map(|m| m.path.as_str()).collect();
            assert_eq!(names, want, "{name}");
        }
    }

    /// #1645 negative: an xz file that is not a tar is not listed as an
    /// archive; it opens as it did before.
    #[test]
    fn a_plain_xz_file_is_not_an_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("notes.txt.xz");
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(b"just some notes\n"), &mut xz).unwrap();
        std::fs::write(&p, xz).unwrap();
        let mut e = crate::widgets::editor::Editor::new();
        e.open(&p).unwrap();
        assert!(e.archive.is_none(), "not a tar, so not an archive");
    }

    /// #1645 negative: a `.tar.xz` whose stream declares more than the cap
    /// is refused before decoding, with the same listing-cap message as a
    /// `.tar.gz`, so the hex viewer says why.
    #[test]
    fn a_tar_xz_past_the_decoded_cap_is_refused_as_a_listing_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("proj-1.0.tar.xz");
        make_tar_xz(&p);
        let err = xz_tar_bytes(&p, 1024).unwrap_err();
        assert!(is_list_cap_refusal(&err.to_string()), "{err}");
        assert!(xz_tar_bytes(&p, TAR_LIST_CAP).is_ok());
    }

    /// #1645: the zip containers by other names route by extension.
    #[test]
    fn zip_based_packages_route_as_zip_by_extension() {
        for name in [
            "app.apk",
            "lib.aar",
            "site.war",
            "app.ear",
            "pkg.nupkg",
            "ext.vsix",
            "book.epub",
        ] {
            assert_eq!(
                kind_from_ext(Path::new(name)),
                Some(ArchiveKind::Zip),
                "{name}"
            );
        }
        assert_eq!(kind_from_ext(Path::new("notes.txt.xz")), None);
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
