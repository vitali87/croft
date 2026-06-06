//! On-disk cache of LSP semantic-token batches, keyed by file content.
//!
//! ## Why this exists
//!
//! Servers that resolve types across modules (ty for Python, vtsls for TS,
//! rust-analyzer for Rust) cannot answer `semanticTokens/full` until they have
//! type-checked the file's import graph. For an import-heavy Python file ty
//! takes ~1.8s on first analysis. During that window the editor paints the
//! tree-sitter base, then the colours visibly "snap" when the server reply
//! lands and recolours every imported name (a `class`/`namespace` token ty
//! paints yellow that tree-sitter could only guess at as a plain identifier).
//!
//! tree-sitter can never close that gap: knowing `BasicStem` is a class or
//! `pydantic` a module requires cross-file resolution, which is exactly the
//! server's job. The only way to kill the first-paint snap is to have the
//! server's answer ready *before* it replies.
//!
//! The server's output is deterministic for identical content (verified: two
//! `semanticTokens/full` requests for the same bytes return byte-identical
//! data), so we persist each full batch keyed by the file's content. On the
//! next open of that exact content - new tab, after a restart, tomorrow - the
//! cached batch is applied synchronously, painting the correct colours on the
//! first frame. The live server reply still arrives ~1.8s later and, being
//! identical, repaints to the same pixels: no visible snap.
//!
//! The cache is best-effort and self-healing. A stale or absent entry only
//! falls back to today's behaviour (tree-sitter base, then the live reply
//! overwrites). It is never authoritative: the live `is_full` batch always
//! replaces it.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Bump when the on-disk format or the meaning of stored tokens changes, so
/// stale entries are ignored rather than misdecoded.
const FORMAT_VERSION: u32 = 1;
const MAGIC: u32 = 0x53_54_43_31; // "STC1"

/// Cap on the number of cached files. When exceeded, the oldest entries by
/// modification time are pruned. Each entry is a few KB, so this bounds the
/// cache at a handful of MB across a large workspace.
const MAX_ENTRIES: usize = 4096;

fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("sem-cache"))
}

/// Stable 64-bit FNV-1a over the key material. Deterministic across runs and
/// toolchain versions (unlike `DefaultHasher`), so a cache written today is
/// still found tomorrow. Collisions are astronomically unlikely and, were one
/// to occur, the mismatched batch is harmless: the live server reply corrects
/// it within the same second it would have anyway.
fn key_hex(path: &Path, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&FORMAT_VERSION.to_le_bytes());
    feed(path.to_string_lossy().as_bytes());
    feed(&[0]);
    feed(text.as_bytes());
    format!("{h:016x}")
}

fn entry_path(path: &Path, text: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{}.stc", key_hex(path, text))))
}

/// Load the cached semantic-token batch for `path` at its current `text`, if
/// one was stored for this exact content. Returns the legend and the flat
/// `semanticTokens/full` `data` array, ready to hand to
/// `Editor::apply_semantic_tokens` with `is_full = true`.
pub fn load(path: &Path, text: &str) -> Option<(Arc<Vec<String>>, Vec<u32>)> {
    let file = entry_path(path, text)?;
    let mut buf = Vec::new();
    fs::File::open(&file).ok()?.read_to_end(&mut buf).ok()?;
    decode(&buf)
}

/// Persist a whole-document semantic-token batch for `path` at `text`. Called
/// only for `is_full` batches over a clean (unmodified) buffer, so the key
/// matches the content that will be read back on the next open. Failures are
/// silently ignored: the cache is an optimisation, never required.
pub fn store(path: &Path, text: &str, legend: &[String], data: &[u32]) {
    if data.is_empty() {
        return;
    }
    let Some(dir) = cache_dir() else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Some(file) = entry_path(path, text) else {
        return;
    };
    let encoded = encode(legend, data);
    // Write to a temp sibling then rename so a crash mid-write never leaves a
    // truncated entry that would decode to garbage.
    let tmp = file.with_extension("stc.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(&encoded).is_ok() && f.flush().is_ok() {
            let _ = fs::rename(&tmp, &file);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    prune(&dir);
}

fn encode(legend: &[String], data: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + legend.len() * 8 + data.len() * 4);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&(legend.len() as u32).to_le_bytes());
    for name in legend {
        let bytes = name.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    for &v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn decode(buf: &[u8]) -> Option<(Arc<Vec<String>>, Vec<u32>)> {
    let mut p = 0usize;
    let take_u32 = |buf: &[u8], p: &mut usize| -> Option<u32> {
        let end = p.checked_add(4)?;
        let v = u32::from_le_bytes(buf.get(*p..end)?.try_into().ok()?);
        *p = end;
        Some(v)
    };
    if take_u32(buf, &mut p)? != MAGIC {
        return None;
    }
    let legend_len = take_u32(buf, &mut p)? as usize;
    let mut legend = Vec::with_capacity(legend_len);
    for _ in 0..legend_len {
        let n = take_u32(buf, &mut p)? as usize;
        let end = p.checked_add(n)?;
        let s = std::str::from_utf8(buf.get(p..end)?).ok()?.to_owned();
        p = end;
        legend.push(s);
    }
    let data_len = take_u32(buf, &mut p)? as usize;
    let mut data = Vec::with_capacity(data_len);
    for _ in 0..data_len {
        data.push(take_u32(buf, &mut p)?);
    }
    Some((Arc::new(legend), data))
}

/// Keep the cache under `MAX_ENTRIES` by deleting the oldest files. Runs after
/// a store (a rare, first-open-only event), so the directory scan is off the
/// hot path.
fn prune(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "stc"))
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            Some((m, e.path()))
        })
        .collect();
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    entries.sort_by_key(|(t, _)| *t);
    let remove = entries.len() - MAX_ENTRIES;
    for (_, p) in entries.into_iter().take(remove) {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_roundtrips() {
        let legend = vec!["namespace".to_string(), "class".to_string()];
        let data = vec![0u32, 1, 9, 1, 0, 2, 0, 4, 0, 0];
        let bytes = encode(&legend, &data);
        let (got_legend, got_data) = decode(&bytes).expect("decodes");
        assert_eq!(&*got_legend, &legend);
        assert_eq!(got_data, data);
    }

    #[test]
    fn decode_rejects_truncated_and_wrong_magic() {
        let bytes = encode(&["x".to_string()], &[0, 0, 1, 0, 0]);
        assert!(decode(&bytes[..bytes.len() - 3]).is_none());
        let mut bad = bytes.clone();
        bad[0] ^= 0xff;
        assert!(decode(&bad).is_none());
    }

    #[test]
    fn key_is_stable_and_content_sensitive() {
        let p = Path::new("/a/b/c.py");
        assert_eq!(key_hex(p, "import os\n"), key_hex(p, "import os\n"));
        assert_ne!(key_hex(p, "import os\n"), key_hex(p, "import sys\n"));
        assert_ne!(key_hex(p, "x"), key_hex(Path::new("/a/b/d.py"), "x"));
    }
}
