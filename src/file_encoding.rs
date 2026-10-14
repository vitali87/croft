//! Which encoding a file without a byte-order mark opens in (#1432).
//!
//! A BOM always wins. Without one, the answer is, in order: the encoding the
//! user last picked for this file through "Reopen with Encoding", the
//! `.editorconfig` `charset`, the `files_encoding` setting (VS Code's
//! `files.encoding`) of the workspace the file is in, and finally UTF-8.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use encoding_rs::Encoding;

/// An encoding as a project declares it: `utf-8-bom` is UTF-8 that is
/// written with a byte-order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charset {
    pub encoding: &'static Encoding,
    pub bom: bool,
}

/// Parse an `.editorconfig` `charset` or a VS Code `files.encoding` value:
/// `latin1`, `utf-8`, `utf-8-bom`, `utf-16le` (VS Code's `utf16le`),
/// `windows1252`, `shiftjis`, `iso88591` and any WHATWG label `encoding_rs`
/// knows. `None` for a value
/// that names no encoding, so a typo never decodes a file as something
/// random.
pub fn parse(value: &str) -> Option<Charset> {
    let value = value.trim().to_ascii_lowercase();
    let squashed: String = value.chars().filter(|c| !matches!(c, '-' | '_')).collect();
    let plain = |encoding| {
        Some(Charset {
            encoding,
            bom: false,
        })
    };
    match squashed.as_str() {
        "" => return None,
        "utf8bom" => {
            return Some(Charset {
                encoding: encoding_rs::UTF_8,
                bom: true,
            });
        }
        // EditorConfig's `latin1`; encoding_rs decodes ISO-8859-1 as
        // windows-1252, as browsers do, which is a superset on printable text.
        "latin1" => return plain(encoding_rs::WINDOWS_1252),
        _ => {}
    }
    if let Some(enc) = Encoding::for_label(value.as_bytes()) {
        return plain(enc);
    }
    // VS Code's ids drop the punctuation WHATWG labels keep.
    let label = if let Some(n) = squashed.strip_prefix("windows") {
        format!("windows-{n}")
    } else if let Some(n) = squashed.strip_prefix("iso8859") {
        format!("iso-8859-{n}")
    } else if let Some(n) = squashed.strip_prefix("cp") {
        if n == "866" {
            String::from("ibm866")
        } else {
            format!("windows-{n}")
        }
    } else {
        match squashed.as_str() {
            "shiftjis" => String::from("shift_jis"),
            "eucjp" => String::from("euc-jp"),
            "euckr" => String::from("euc-kr"),
            "koi8r" => String::from("koi8-r"),
            "koi8u" => String::from("koi8-u"),
            "big5hkscs" => String::from("big5"),
            "macroman" => String::from("macintosh"),
            "utf16le" => String::from("utf-16le"),
            "utf16be" => String::from("utf-16be"),
            _ => squashed.clone(),
        }
    };
    Encoding::for_label(label.as_bytes()).and_then(plain)
}

/// `files_encoding` per workspace root, as last applied from settings.
static WORKSPACE: RwLock<Vec<(PathBuf, Charset)>> = RwLock::new(Vec::new());

/// Set (or clear, with `None`) the `files_encoding` default for files under
/// `root`.
pub fn set_workspace_default(root: &Path, charset: Option<Charset>) {
    let mut all = WORKSPACE.write().unwrap_or_else(|e| e.into_inner());
    all.retain(|(r, _)| r != root);
    if let Some(charset) = charset {
        all.push((root.to_path_buf(), charset));
    }
}

fn workspace_default(path: &Path) -> Option<Charset> {
    let all = WORKSPACE.read().unwrap_or_else(|e| e.into_inner());
    all.iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.as_os_str().len())
        .map(|(_, charset)| *charset)
}

/// Encodings picked by hand, by canonical path. Kept across restarts in
/// `~/.cache/croft/encodings.json`, bounded so it never grows without end.
fn remembered_map() -> &'static Mutex<BTreeMap<PathBuf, &'static Encoding>> {
    static MAP: OnceLock<Mutex<BTreeMap<PathBuf, &'static Encoding>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(load_remembered()))
}

const REMEMBERED_CAP: usize = 1000;

fn remembered_file() -> Option<PathBuf> {
    // Tests never read or write the user's real cache.
    if cfg!(test) {
        return None;
    }
    Some(crate::session_state::dirs_cache_croft().join("encodings.json"))
}

fn load_remembered() -> BTreeMap<PathBuf, &'static Encoding> {
    let Some(file) = remembered_file() else {
        return BTreeMap::new();
    };
    let Ok(json) = std::fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    let names: BTreeMap<PathBuf, String> = serde_json::from_str(&json).unwrap_or_default();
    names
        .into_iter()
        .filter_map(|(path, name)| Some((path, Encoding::for_label(name.as_bytes())?)))
        .collect()
}

fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Remember that the user reopened `path` in `enc`, so the next open of the
/// file uses it too.
pub fn remember(path: &Path, enc: &'static Encoding) {
    let mut map = remembered_map().lock().unwrap_or_else(|e| e.into_inner());
    map.insert(key(path), enc);
    while map.len() > REMEMBERED_CAP {
        let Some(first) = map.keys().next().cloned() else {
            break;
        };
        map.remove(&first);
    }
    let Some(file) = remembered_file() else {
        return;
    };
    let names: BTreeMap<&PathBuf, &str> = map.iter().map(|(p, e)| (p, e.name())).collect();
    if let Ok(json) = serde_json::to_string(&names) {
        let _ = crate::session_state::write_private_atomically(&file, json.as_bytes());
    }
}

fn remembered(path: &Path) -> Option<&'static Encoding> {
    let map = remembered_map().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&key(path)).copied()
}

/// The encoding to decode `path` in when its bytes carry no BOM, or `None`
/// for plain UTF-8. `editorconfig` is the file's resolved `.editorconfig`.
pub fn declared_for(path: &Path, editorconfig: &crate::editorconfig::Props) -> Option<Charset> {
    remembered(path)
        .map(|encoding| Charset {
            encoding,
            bom: false,
        })
        .or(editorconfig.charset)
        .or_else(|| workspace_default(path))
        .or_else(|| workspace_default(&key(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(value: &str) -> Option<&'static str> {
        parse(value).map(|c| c.encoding.name())
    }

    #[test]
    fn editorconfig_and_vs_code_names_parse() {
        assert_eq!(enc("latin1"), Some("windows-1252"));
        assert_eq!(enc("utf-8"), Some("UTF-8"));
        assert_eq!(enc("utf-16le"), Some("UTF-16LE"));
        assert_eq!(enc("utf-16be"), Some("UTF-16BE"));
        assert_eq!(enc("utf16le"), Some("UTF-16LE"));
        assert_eq!(enc("utf16be"), Some("UTF-16BE"));
        assert_eq!(enc("windows1252"), Some("windows-1252"));
        assert_eq!(enc("windows1251"), Some("windows-1251"));
        assert_eq!(enc("iso88591"), Some("windows-1252"));
        assert_eq!(enc("iso885915"), Some("ISO-8859-15"));
        assert_eq!(enc("shiftjis"), Some("Shift_JIS"));
        assert_eq!(enc("Shift_JIS"), Some("Shift_JIS"));
        assert_eq!(enc("eucjp"), Some("EUC-JP"));
        assert_eq!(enc("gbk"), Some("GBK"));
        assert_eq!(enc("koi8r"), Some("KOI8-R"));
        assert_eq!(enc("cp866"), Some("IBM866"));
        let bom = parse("utf-8-bom").unwrap();
        assert_eq!((bom.encoding.name(), bom.bom), ("UTF-8", true));
        assert_eq!(parse("utf8bom"), Some(bom));
        assert!(!parse("latin1").unwrap().bom);
    }

    #[test]
    fn a_value_that_names_no_encoding_is_ignored() {
        for v in ["", "  ", "latin9000", "utf-7-bom", "windowsXP", "auto"] {
            assert_eq!(parse(v), None, "{v:?}");
        }
    }

    #[test]
    fn the_innermost_workspace_default_applies() {
        let root = std::env::temp_dir().join(format!("croft-enc-ws-{}", std::process::id()));
        let inner = root.join("legacy");
        set_workspace_default(&root, parse("shiftjis"));
        set_workspace_default(&inner, parse("windows1252"));
        let none = crate::editorconfig::Props::default();
        let name = |p: &Path| declared_for(p, &none).map(|c| c.encoding.name());
        assert_eq!(name(&root.join("a.txt")), Some("Shift_JIS"));
        assert_eq!(name(&inner.join("b.txt")), Some("windows-1252"));
        assert_eq!(name(Path::new("/elsewhere/c.txt")), None);
        set_workspace_default(&inner, None);
        assert_eq!(name(&inner.join("b.txt")), Some("Shift_JIS"));
        set_workspace_default(&root, None);
        assert_eq!(name(&root.join("a.txt")), None);
    }

    #[test]
    fn a_remembered_choice_beats_editorconfig_and_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"x").unwrap();
        set_workspace_default(dir.path(), parse("koi8r"));
        let ec = crate::editorconfig::Props {
            charset: parse("latin1"),
            ..Default::default()
        };
        assert_eq!(
            declared_for(&file, &ec).unwrap().encoding.name(),
            "windows-1252"
        );
        let none = crate::editorconfig::Props::default();
        assert_eq!(
            declared_for(&file, &none).unwrap().encoding.name(),
            "KOI8-R"
        );
        remember(&file, encoding_rs::SHIFT_JIS);
        assert_eq!(
            declared_for(&file, &ec).unwrap().encoding.name(),
            "Shift_JIS"
        );
        set_workspace_default(dir.path(), None);
    }
}
