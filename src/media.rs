//! Audio/video info view (#183): pure-Rust HEADER parsers for the
//! common containers - WAV (RIFF fmt), FLAC (STREAMINFO), MP3 (ID3v2
//! text tags + first MPEG frame), MP4/M4A/MOV (mvhd + tkhd) - reading
//! bounded byte windows, never decoding streams. The view renders as
//! markdown through the preview machinery, with an optional poster
//! frame via ffmpeg when it exists on PATH (the pdftoppm pattern).

use std::io::Read as _;
use std::path::Path;

/// Bytes read from the head (and tail for some formats): headers only.
const HEAD_BYTES: usize = 512 * 1024;

#[derive(Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub format: String,
    pub codec: Option<String>,
    pub duration_s: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub tags: Vec<(String, String)>,
}

pub fn extension_is_media(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp3" | "wav" | "flac" | "m4a" | "mp4" | "mov" | "m4v"
    )
}

/// Parse what the head declares. `None` when nothing recognisable.
pub fn probe(path: &Path) -> Option<MediaInfo> {
    let len = std::fs::metadata(path).ok()?.len();
    let mut head = vec![0u8; HEAD_BYTES.min(len as usize)];
    let mut f = std::fs::File::open(path).ok()?;
    f.read_exact(&mut head).ok()?;
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
        return parse_wav(&head, len);
    }
    if head.starts_with(b"fLaC") {
        return parse_flac(&head);
    }
    if head.starts_with(b"ID3")
        || head.get(0..2) == Some(&[0xff, 0xfb])
        || head.get(0..2) == Some(&[0xff, 0xf3])
    {
        return parse_mp3(&head, len);
    }
    if head.get(4..8) == Some(b"ftyp") {
        let brand = head
            .get(8..12)
            .map(|b| String::from_utf8_lossy(b).trim().to_string());
        return parse_mp4(&head, brand);
    }
    None
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn le16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn parse_wav(head: &[u8], file_len: u64) -> Option<MediaInfo> {
    // Walk RIFF chunks for fmt and data.
    let mut i = 12usize;
    let mut info = MediaInfo {
        format: String::from("WAV (PCM audio)"),
        ..Default::default()
    };
    let mut byte_rate = 0u32;
    let mut data_len = None;
    while i + 8 <= head.len() {
        let id = &head[i..i + 4];
        let sz = le32(head, i + 4)? as usize;
        if id == b"fmt " {
            info.codec = le16(head, i + 8).map(|tag| match tag {
                1 => String::from("PCM"),
                3 => String::from("IEEE float"),
                6 => String::from("A-law"),
                7 => String::from("mu-law"),
                85 => String::from("MP3"),
                other => format!("format tag {other}"),
            });
            info.channels = le16(head, i + 10);
            info.sample_rate = le32(head, i + 12);
            byte_rate = le32(head, i + 16).unwrap_or(0);
        } else if id == b"data" {
            data_len = Some(sz as u64);
        }
        i += 8 + sz + (sz & 1);
    }
    let data = data_len.unwrap_or_else(|| file_len.saturating_sub(44));
    if byte_rate > 0 {
        info.duration_s = Some(data as f64 / byte_rate as f64);
        info.bitrate_kbps = Some(byte_rate * 8 / 1000);
    }
    Some(info)
}

fn parse_flac(head: &[u8]) -> Option<MediaInfo> {
    // STREAMINFO is the first metadata block: 34 bytes at offset 8.
    let b = head.get(8..8 + 34)?;
    let sample_rate = ((b[10] as u32) << 12) | ((b[11] as u32) << 4) | ((b[12] as u32) >> 4);
    let channels = (((b[12] >> 1) & 0x07) + 1) as u16;
    let total: u64 = (((b[13] & 0x0f) as u64) << 32)
        | ((b[14] as u64) << 24)
        | ((b[15] as u64) << 16)
        | ((b[16] as u64) << 8)
        | (b[17] as u64);
    Some(MediaInfo {
        format: String::from("FLAC (lossless audio)"),
        codec: Some(String::from("FLAC")),
        duration_s: (sample_rate > 0 && total > 0).then(|| total as f64 / sample_rate as f64),
        sample_rate: Some(sample_rate),
        channels: Some(channels),
        ..Default::default()
    })
}

fn parse_mp3(head: &[u8], file_len: u64) -> Option<MediaInfo> {
    let mut info = MediaInfo {
        format: String::from("MP3 (MPEG audio)"),
        codec: Some(String::from("MPEG Layer III")),
        ..Default::default()
    };
    let mut off = 0usize;
    if head.starts_with(b"ID3") {
        let size = (((head[6] & 0x7f) as usize) << 21)
            | (((head[7] & 0x7f) as usize) << 14)
            | (((head[8] & 0x7f) as usize) << 7)
            | ((head[9] & 0x7f) as usize);
        // Text frames: TIT2/TPE1/TALB, ID3v2.3+ layout.
        let mut i = 10usize;
        let end = (10 + size).min(head.len());
        while i + 10 < end {
            let id = &head[i..i + 4];
            let fsz = be32(head, i + 4)? as usize;
            if fsz == 0 || i + 10 + fsz > end {
                break;
            }
            if let Ok(name) = std::str::from_utf8(id)
                && matches!(name, "TIT2" | "TPE1" | "TALB")
            {
                let body = &head[i + 10..i + 10 + fsz];
                // The first byte declares the ENCODING (#202 review):
                // strip it unconditionally and decode accordingly, or a
                // UTF-8/UTF-16 tag keeps a stray control byte.
                let text = match body.split_first() {
                    Some((0, rest)) => encoding_rs::WINDOWS_1252.decode(rest).0.into_owned(),
                    Some((1, rest)) => {
                        let (enc, skip) = match rest.get(0..2) {
                            Some([0xfe, 0xff]) => (encoding_rs::UTF_16BE, 2),
                            Some([0xff, 0xfe]) => (encoding_rs::UTF_16LE, 2),
                            _ => (encoding_rs::UTF_16LE, 0),
                        };
                        enc.decode(&rest[skip..]).0.into_owned()
                    }
                    Some((2, rest)) => encoding_rs::UTF_16BE.decode(rest).0.into_owned(),
                    Some((3, rest)) => String::from_utf8_lossy(rest).into_owned(),
                    _ => String::new(),
                };
                let label = match name {
                    "TIT2" => "title",
                    "TPE1" => "artist",
                    _ => "album",
                };
                let clean = text.trim_matches('\0').trim().to_string();
                if !clean.is_empty() {
                    info.tags.push((label.to_string(), clean));
                }
            }
            i += 10 + fsz;
        }
        off = 10 + size;
    }
    // First MPEG frame header after the tag.
    const BITRATES: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const RATES: [u32; 4] = [44100, 48000, 32000, 0];
    while off + 4 <= head.len() {
        if head[off] == 0xff && (head[off + 1] & 0xe0) == 0xe0 {
            let bitrate = BITRATES[(head[off + 2] >> 4) as usize];
            let rate = RATES[((head[off + 2] >> 2) & 0x03) as usize];
            if bitrate > 0 && rate > 0 {
                info.bitrate_kbps = Some(bitrate);
                info.sample_rate = Some(rate);
                info.duration_s = Some(
                    (file_len.saturating_sub(off as u64)) as f64 * 8.0 / (bitrate as f64 * 1000.0),
                );
            }
            break;
        }
        off += 1;
    }
    Some(info)
}

fn parse_mp4(head: &[u8], brand: Option<String>) -> Option<MediaInfo> {
    let mut info = MediaInfo {
        format: String::from("MP4/MOV container"),
        codec: brand.map(|b| format!("brand {b}")),
        ..Default::default()
    };
    // Box walk: find moov/mvhd (timescale+duration) and any tkhd dims.
    fn walk(b: &[u8], info: &mut MediaInfo, depth: usize) {
        if depth > 6 {
            return;
        }
        let mut i = 0usize;
        while i + 8 <= b.len() {
            let sz = u32::from_be_bytes(b[i..i + 4].try_into().unwrap()) as usize;
            let typ = &b[i + 4..i + 8];
            if sz < 8 || i + sz > b.len() {
                break;
            }
            let body = &b[i + 8..i + sz];
            match typ {
                b"moov" | b"trak" => walk(body, info, depth + 1),
                b"mvhd" if body.len() >= 20 => {
                    let (ts, dur) = if body[0] == 1 {
                        (
                            u32::from_be_bytes(body[20..24].try_into().unwrap()),
                            u64::from_be_bytes(body[24..32].try_into().unwrap()),
                        )
                    } else {
                        (
                            u32::from_be_bytes(body[12..16].try_into().unwrap()),
                            u32::from_be_bytes(body[16..20].try_into().unwrap()) as u64,
                        )
                    };
                    if ts > 0 {
                        info.duration_s = Some(dur as f64 / ts as f64);
                    }
                }
                b"tkhd" if body.len() >= 84 => {
                    let w = u32::from_be_bytes(
                        body[body.len() - 8..body.len() - 4].try_into().unwrap(),
                    ) >> 16;
                    let h = u32::from_be_bytes(body[body.len() - 4..].try_into().unwrap()) >> 16;
                    if w > 0 && h > 0 {
                        info.width = Some(w);
                        info.height = Some(h);
                    }
                }
                _ => {}
            }
            i += sz;
        }
    }
    walk(head, &mut info, 0);
    Some(info)
}

/// Render the info card as markdown, with a poster frame extracted via
/// ffmpeg when it exists on PATH (optional external tool, the pdftoppm
/// pattern; graceful absence).
pub fn to_markdown(path: &Path, info: &MediaInfo, scratch: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut md = format!(
        "# {name}

"
    );
    md.push_str(
        "|property|value|
|---|---|
",
    );
    md.push_str(&format!(
        "|format|{}|
",
        info.format
    ));
    let esc = |t: &str| t.replace('\\', "\\\\").replace('|', "\\|");
    if let Some(c) = &info.codec {
        md.push_str(&format!(
            "|codec|{}|
",
            esc(c)
        ));
    }
    if let Some(d) = info.duration_s {
        let m = (d / 60.0).floor() as u64;
        md.push_str(&format!(
            "|duration|{m}:{:04.1}|
",
            d - (m as f64) * 60.0
        ));
    }
    if let (Some(w), Some(h)) = (info.width, info.height) {
        md.push_str(&format!(
            "|dimensions|{w} x {h}|
"
        ));
    }
    if let Some(r) = info.sample_rate {
        md.push_str(&format!(
            "|sample rate|{r} Hz|
"
        ));
    }
    if let Some(c) = info.channels {
        md.push_str(&format!(
            "|channels|{c}|
"
        ));
    }
    if let Some(b) = info.bitrate_kbps {
        md.push_str(&format!(
            "|bitrate|{b} kbps|
"
        ));
    }
    if let Ok(meta) = std::fs::metadata(path) {
        md.push_str(&format!(
            "|size|{} bytes|
",
            meta.len()
        ));
    }
    for (k, v) in &info.tags {
        // Escape pipes/backslashes (#202 review): tag text is arbitrary
        // and must not restructure the synthesised table.
        md.push_str(&format!(
            "|{}|{}|
",
            esc(k),
            esc(v)
        ));
    }
    md.push('\n');
    // Poster frame for video files, when ffmpeg exists.
    if info.width.is_some()
        && let Some(poster) = poster_frame(path, scratch)
    {
        md.push_str(&format!(
            "![poster]({})

",
            poster.display()
        ));
    }
    md.push_str(
        "Playback is not available in a terminal. Media: Open in System Player (palette) hands the file to your OS.\n",
    );
    md
}

fn poster_frame(path: &Path, scratch: &Path) -> Option<std::path::PathBuf> {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let meta = std::fs::metadata(path).ok()?;
    meta.len().hash(&mut h);
    // mtime joins the key (#202 review): a re-encode landing at the
    // same byte length must not serve a stale poster forever.
    if let Ok(m) = meta.modified()
        && let Ok(d) = m.duration_since(std::time::UNIX_EPOCH)
    {
        d.as_nanos().hash(&mut h);
    }
    let out = scratch.join(format!("poster-{:016x}.png", h.finish()));
    if out.is_file() {
        return Some(out);
    }
    std::fs::create_dir_all(scratch).ok()?;
    let tmp = scratch.join(format!(
        "tmp-{}-{:016x}.png",
        std::process::id(),
        h.finish()
    ));
    // Bounded (#202 review): poll with try_wait and kill+reap on the
    // deadline, so a wedged ffmpeg cannot stall the open. First open of
    // a given video only - the poster is hash-cached thereafter.
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "quiet", "-ss", "1", "-i"])
        .arg(path)
        .args(["-frames:v", "1"])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        // Never the inherited tty: this runs mid-session, and anything a
        // child writes there paints raw bytes over the TUI frame.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    // Rename-on-success (#202 review): a killed or failed ffmpeg can
    // leave a partial file, which must never be served from the cache.
    if ok && tmp.is_file() && std::fs::rename(&tmp, &out).is_ok() {
        return Some(out);
    }
    let _ = std::fs::remove_file(&tmp);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn wav_header_yields_duration_and_rates() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.wav");
        let mut f = std::fs::File::create(&p).unwrap();
        let sample_rate = 8000u32;
        let byte_rate = sample_rate * 2;
        let data = vec![0u8; (byte_rate * 2) as usize]; // 2 seconds
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"RIFF");
        hdr.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        hdr.extend_from_slice(b"WAVEfmt ");
        hdr.extend_from_slice(&16u32.to_le_bytes());
        hdr.extend_from_slice(&1u16.to_le_bytes());
        hdr.extend_from_slice(&1u16.to_le_bytes());
        hdr.extend_from_slice(&sample_rate.to_le_bytes());
        hdr.extend_from_slice(&byte_rate.to_le_bytes());
        hdr.extend_from_slice(&2u16.to_le_bytes());
        hdr.extend_from_slice(&16u16.to_le_bytes());
        hdr.extend_from_slice(b"data");
        hdr.extend_from_slice(&(data.len() as u32).to_le_bytes());
        f.write_all(&hdr).unwrap();
        f.write_all(&data).unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.sample_rate, Some(8000));
        assert_eq!(info.channels, Some(1));
        assert!((info.duration_s.unwrap() - 2.0).abs() < 0.01);
    }

    #[test]
    fn flac_streaminfo_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.flac");
        let mut b = Vec::new();
        b.extend_from_slice(b"fLaC");
        b.extend_from_slice(&[0x80, 0, 0, 34]); // last block, STREAMINFO, len 34
        let mut si = [0u8; 34];
        // sample rate 44100 = 0xAC44 -> bits [10..12]+high nibble of 12
        si[10] = 0x0A;
        si[11] = 0xC4;
        si[12] = 0x40 | ((2 - 1) << 1); // stereo
        // total samples = 44100 (1 second)
        si[14] = 0x00;
        si[15] = 0x00;
        si[16] = 0xAC;
        si[17] = 0x44;
        b.extend_from_slice(&si);
        std::fs::write(&p, &b).unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.sample_rate, Some(44100));
        assert_eq!(info.channels, Some(2));
        assert!((info.duration_s.unwrap() - 1.0).abs() < 0.001);
    }

    #[test]
    fn mp3_id3_tags_and_frame_header_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.mp3");
        let mut b = Vec::new();
        let title = b"Song Name";
        let frame_size = 1 + title.len();
        let tag_size = 10 + frame_size;
        b.extend_from_slice(b"ID3\x03\x00\x00");
        b.extend_from_slice(&[
            ((tag_size >> 21) & 0x7f) as u8,
            ((tag_size >> 14) & 0x7f) as u8,
            ((tag_size >> 7) & 0x7f) as u8,
            (tag_size & 0x7f) as u8,
        ]);
        b.extend_from_slice(b"TIT2");
        b.extend_from_slice(&(frame_size as u32).to_be_bytes());
        b.extend_from_slice(&[0, 0]);
        b.push(0);
        b.extend_from_slice(title);
        // pad to declared tag size then a 128kbps 44.1k frame header
        while b.len() < 10 + tag_size {
            b.push(0);
        }
        b.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        b.extend_from_slice(&vec![0u8; 32000]); // ~2s at 128kbps
        std::fs::write(&p, &b).unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.bitrate_kbps, Some(128));
        assert_eq!(info.sample_rate, Some(44100));
        assert!(
            info.tags
                .iter()
                .any(|(k, v)| k == "title" && v == "Song Name")
        );
        assert!((info.duration_s.unwrap() - 2.0).abs() < 0.1);
    }

    #[test]
    fn mp4_mvhd_and_tkhd_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.mp4");
        let mut mvhd = vec![0u8; 100];
        mvhd[..4].copy_from_slice(&108u32.to_be_bytes());
        mvhd[4..8].copy_from_slice(b"mvhd");
        // version 0: timescale at body[12..16], duration at [16..20]
        mvhd[8 + 12..8 + 16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[8 + 16..8 + 20].copy_from_slice(&5000u32.to_be_bytes());
        let mut tkhd = vec![0u8; 100];
        tkhd[..4].copy_from_slice(&100u32.to_be_bytes());
        tkhd[4..8].copy_from_slice(b"tkhd");
        let bl = 100 - 8;
        tkhd[8 + bl - 8..8 + bl - 4].copy_from_slice(&(640u32 << 16).to_be_bytes());
        tkhd[8 + bl - 4..].copy_from_slice(&(480u32 << 16).to_be_bytes());
        mvhd.truncate(108);
        let trak_len = 8 + tkhd.len();
        let moov_len = 8 + mvhd.len() + trak_len;
        let mut b = Vec::new();
        b.extend_from_slice(&16u32.to_be_bytes());
        b.extend_from_slice(b"ftypisom");
        b.extend_from_slice(&[0u8; 4]);
        b.extend_from_slice(&(moov_len as u32).to_be_bytes());
        b.extend_from_slice(b"moov");
        b.extend_from_slice(&mvhd);
        b.extend_from_slice(&(trak_len as u32).to_be_bytes());
        b.extend_from_slice(b"trak");
        b.extend_from_slice(&tkhd);
        std::fs::write(&p, &b).unwrap();
        let info = probe(&p).unwrap();
        assert!((info.duration_s.unwrap() - 5.0).abs() < 0.001);
        assert_eq!(info.width, Some(640));
        assert_eq!(info.height, Some(480));
    }

    #[test]
    fn junk_is_not_media() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.mp3");
        std::fs::write(&p, b"nothing here").unwrap();
        assert!(probe(&p).is_none());
    }
}
