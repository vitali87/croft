//! Content-based format detection (#174): magic-byte signatures as the
//! SECOND routing hint behind file extensions. An extensionless PNG, a
//! `.dat` that is a zip, or a renamed PDF used to land in the text/hex
//! fallback; sniffing the bytes the open already holds routes them to
//! their real viewer. Pure table + function, no IO.

/// A recognised container/format signature. Only formats croft can DO
/// something with today get a variant; new viewers in the format epic
/// (#184) extend this as they land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magic {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Pdf,
    /// Any zip container — an Office document, a jar, or a plain
    /// archive; the caller decides what to try (xlsx via the sheet
    /// viewer first, hex otherwise).
    Zip,
    Gzip,
    Tar,
    Sqlite,
}

impl Magic {
    pub fn is_image(self) -> bool {
        matches!(
            self,
            Magic::Png | Magic::Jpeg | Magic::Gif | Magic::WebP | Magic::Bmp
        )
    }
}

/// Sniff the leading bytes (a 4 KiB head is plenty: the longest offset
/// checked is tar's magic at 257). Returns `None` for anything
/// unrecognised — the caller falls back to the text/binary heuristics.
pub fn sniff(bytes: &[u8]) -> Option<Magic> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Magic::Png);
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(Magic::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Magic::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(Magic::WebP);
    }
    if bytes.starts_with(b"%PDF-") {
        return Some(Magic::Pdf);
    }
    // Zip local-file header, plus the empty-archive and spanned markers.
    if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        return Some(Magic::Zip);
    }
    if bytes.starts_with(b"\x1f\x8b") {
        return Some(Magic::Gzip);
    }
    if bytes.starts_with(b"SQLite format 3\0") {
        return Some(Magic::Sqlite);
    }
    // Tar has no leading magic: "ustar" sits at offset 257 (both the
    // POSIX "ustar\0" and the GNU "ustar " variants).
    if bytes.len() > 262 && &bytes[257..262] == b"ustar" {
        return Some(Magic::Tar);
    }
    // BMP last: "BM" is only two bytes, the weakest signature here, so
    // every stronger prefix gets its chance first.
    if bytes.len() >= 6 && bytes.starts_with(b"BM") {
        return Some(Magic::Bmp);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_each_signature() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....."), Some(Magic::Png));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0..JFIF"), Some(Magic::Jpeg));
        assert_eq!(sniff(b"GIF89a......"), Some(Magic::Gif));
        assert_eq!(sniff(b"GIF87a......"), Some(Magic::Gif));
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some(Magic::WebP));
        assert_eq!(sniff(b"BM\x36\x00\x00\x00"), Some(Magic::Bmp));
        assert_eq!(sniff(b"%PDF-1.7\n%\xe2\xe3"), Some(Magic::Pdf));
        assert_eq!(sniff(b"PK\x03\x04\x14\x00"), Some(Magic::Zip));
        assert_eq!(sniff(b"PK\x05\x06\x00\x00"), Some(Magic::Zip), "empty zip");
        assert_eq!(sniff(b"\x1f\x8b\x08\x00"), Some(Magic::Gzip));
        assert_eq!(sniff(b"SQLite format 3\0"), Some(Magic::Sqlite));
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&tar), Some(Magic::Tar));
    }

    #[test]
    fn near_misses_and_short_heads_stay_unrecognised() {
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"BM"), None, "two lone bytes prove nothing");
        assert_eq!(sniff(b"%PDF"), None, "truncated pdf marker");
        assert_eq!(
            sniff(b"RIFF\x10\x00\x00\x00WAVE"),
            None,
            "RIFF but not WEBP"
        );
        assert_eq!(sniff(b"PK\x01\x02"), None, "central-dir record alone");
        assert_eq!(sniff(b"plain text here"), None);
        assert_eq!(sniff(b"SQLite format 4\0"), None);
        let mut not_tar = vec![0u8; 512];
        not_tar[257..262].copy_from_slice(b"nope!");
        assert_eq!(sniff(&not_tar), None);
    }

    #[test]
    fn stronger_prefixes_win_over_bmp() {
        // "BM" never shadows a real signature that begins differently;
        // this pins the ordering contract.
        assert!(sniff(b"BMP is weak").is_some());
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nBM"), Some(Magic::Png));
    }

    #[test]
    fn image_classification_covers_exactly_the_image_variants() {
        for (m, img) in [
            (Magic::Png, true),
            (Magic::Jpeg, true),
            (Magic::Gif, true),
            (Magic::WebP, true),
            (Magic::Bmp, true),
            (Magic::Pdf, false),
            (Magic::Zip, false),
            (Magic::Gzip, false),
            (Magic::Tar, false),
            (Magic::Sqlite, false),
        ] {
            assert_eq!(m.is_image(), img, "{m:?}");
        }
    }
}
