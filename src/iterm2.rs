use anyhow::{Context, Result};
use plist::{Dictionary, Value};
use std::path::{Path, PathBuf};

pub const ITERM2_PLIST_REL: &str = "Library/Preferences/com.googlecode.iterm2.plist";

/// PostScript name iTerm2 stores in `Normal Font` and `Non Ascii Font`.
/// Format is "<PostScriptName> <size>".
pub fn primary_font_value(font_ps: &str, size: u32) -> String {
    format!("{font_ps} {size}")
}

#[derive(Debug, thiserror::Error)]
pub enum ITerm2Error {
    #[error("iTerm2 plist not found at {0}; install and launch iTerm2 first")]
    PlistMissing(PathBuf),
    #[error("iTerm2 plist has no `Default Bookmark Guid` (the default profile)")]
    NoDefaultGuid,
    #[error("iTerm2 plist has no profile matching the default GUID `{0}`")]
    NoMatchingProfile(String),
    #[error("iTerm2 plist top level is not a dictionary")]
    NotADictionary,
    #[error("`New Bookmarks` is missing or not an array")]
    NoBookmarksArray,
}

/// Apply font settings to the *default profile* in an iTerm2 plist value.
/// Pure function: no I/O, mutates the value in place.
pub fn apply_font_settings(
    plist: &mut Value,
    primary_font_ps: &str,
    nonascii_font_ps: &str,
    size: u32,
) -> Result<(), ITerm2Error> {
    let dict = plist
        .as_dictionary_mut()
        .ok_or(ITerm2Error::NotADictionary)?;

    let default_guid = dict
        .get("Default Bookmark Guid")
        .and_then(|v| v.as_string())
        .ok_or(ITerm2Error::NoDefaultGuid)?
        .to_string();

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;

    let profile = bookmarks
        .iter_mut()
        .filter_map(|v| v.as_dictionary_mut())
        .find(|d| d.get("Guid").and_then(|g| g.as_string()) == Some(&default_guid))
        .ok_or_else(|| ITerm2Error::NoMatchingProfile(default_guid.clone()))?;

    set_string(profile, "Normal Font", primary_font_value(primary_font_ps, size));
    set_string(
        profile,
        "Non Ascii Font",
        primary_font_value(nonascii_font_ps, size),
    );
    profile.insert("Use Non-ASCII Font".into(), Value::Boolean(true));
    profile.insert(
        "Non-ASCII Anti Aliased".into(),
        Value::Boolean(true),
    );
    Ok(())
}

fn set_string(dict: &mut Dictionary, key: &str, value: String) {
    dict.insert(key.into(), Value::String(value));
}

/// Load → mutate → save the iTerm2 plist on disk.
pub fn install_font_settings(
    plist_path: &Path,
    primary_font_ps: &str,
    nonascii_font_ps: &str,
    size: u32,
) -> Result<()> {
    if !plist_path.exists() {
        return Err(ITerm2Error::PlistMissing(plist_path.to_path_buf()).into());
    }
    let mut value: Value = Value::from_file(plist_path)
        .with_context(|| format!("reading {}", plist_path.display()))?;
    apply_font_settings(&mut value, primary_font_ps, nonascii_font_ps, size)?;
    value
        .to_file_xml(plist_path)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    Ok(())
}

pub fn default_plist_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(ITERM2_PLIST_REL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    fn synth_plist(default_guid: &str, profile_guids: &[&str]) -> Value {
        let mut bookmarks: Vec<Value> = Vec::new();
        for g in profile_guids {
            let mut d = Dictionary::new();
            d.insert("Guid".into(), Value::String((*g).to_string()));
            d.insert("Name".into(), Value::String(format!("Profile {g}")));
            bookmarks.push(Value::Dictionary(d));
        }
        let mut top = Dictionary::new();
        top.insert(
            "Default Bookmark Guid".into(),
            Value::String(default_guid.to_string()),
        );
        top.insert("New Bookmarks".into(), Value::Array(bookmarks));
        Value::Dictionary(top)
    }

    fn profile_in<'a>(plist: &'a Value, guid: &str) -> &'a Dictionary {
        plist
            .as_dictionary()
            .unwrap()
            .get("New Bookmarks")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|p| {
                p.as_dictionary()
                    .and_then(|d| d.get("Guid"))
                    .and_then(|v| v.as_string())
                    == Some(guid)
            })
            .unwrap()
            .as_dictionary()
            .unwrap()
    }

    #[test]
    fn primary_font_value_concatenates_postscript_name_and_size() {
        assert_eq!(primary_font_value("MesloLGSNFM-Regular", 13), "MesloLGSNFM-Regular 13");
        assert_eq!(primary_font_value("FiraCodeNFM-Reg", 14), "FiraCodeNFM-Reg 14");
    }

    #[test]
    fn apply_font_settings_writes_normal_and_nonascii_font() {
        let mut plist = synth_plist("DEFAULT-GUID", &["OTHER-GUID", "DEFAULT-GUID"]);
        apply_font_settings(&mut plist, "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let p = profile_in(&plist, "DEFAULT-GUID");
        assert_eq!(
            p.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
        assert_eq!(
            p.get("Non Ascii Font").unwrap().as_string(),
            Some("SymbolsNFM 13")
        );
    }

    #[test]
    fn apply_font_settings_enables_use_non_ascii_font() {
        let mut plist = synth_plist("G1", &["G1"]);
        apply_font_settings(&mut plist, "X-Reg", "Y-Reg", 12).unwrap();
        let p = profile_in(&plist, "G1");
        assert_eq!(p.get("Use Non-ASCII Font").unwrap().as_boolean(), Some(true));
    }

    #[test]
    fn apply_font_settings_only_touches_default_profile() {
        let mut plist = synth_plist("DEFAULT-GUID", &["OTHER-GUID", "DEFAULT-GUID"]);
        apply_font_settings(&mut plist, "F-Reg", "S-Reg", 13).unwrap();
        let other = profile_in(&plist, "OTHER-GUID");
        assert!(other.get("Normal Font").is_none(), "other profile should be untouched");
        let defaultp = profile_in(&plist, "DEFAULT-GUID");
        assert!(defaultp.get("Normal Font").is_some());
    }

    #[test]
    fn apply_font_settings_errors_when_no_default_guid() {
        let mut top = Dictionary::new();
        top.insert("New Bookmarks".into(), Value::Array(vec![]));
        let mut plist = Value::Dictionary(top);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NoDefaultGuid));
    }

    #[test]
    fn apply_font_settings_errors_when_default_guid_does_not_match_any_profile() {
        let mut plist = synth_plist("MISSING-GUID", &["A", "B"]);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NoMatchingProfile(g) if g == "MISSING-GUID"));
    }

    #[test]
    fn apply_font_settings_errors_when_top_level_is_not_dict() {
        let mut plist = Value::Array(vec![]);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NotADictionary));
    }

    #[test]
    fn install_font_settings_round_trips_through_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let plist = synth_plist("GUID-1", &["GUID-1"]);
        plist.to_file_xml(tmp.path()).unwrap();
        install_font_settings(tmp.path(), "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let reloaded: Value = Value::from_file(tmp.path()).unwrap();
        let p = profile_in(&reloaded, "GUID-1");
        assert_eq!(
            p.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
    }

    #[test]
    fn install_font_settings_errors_when_plist_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("nonexistent.plist");
        let err = install_font_settings(&bogus, "F", "S", 13).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not found"), "expected 'not found' message, got: {msg}");
    }
}
