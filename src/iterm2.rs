use anyhow::{Context, Result};
use plist::{Dictionary, Value};
use std::path::{Path, PathBuf};

pub const ITERM2_PLIST_REL: &str = "Library/Preferences/com.googlecode.iterm2.plist";
const CMD_SHIFT_F_KEY: &str = "0x46-0x120000-0x3";
const CMD_SHIFT_F_HEX: &str = "0x1b 0x5b 0x37 0x30 0x3b 0x31 0x30 0x75";
const CMD_V_KEY: &str = "0x76-0x100000-0x9";
const CMD_V_HEX: &str = "0x1b 0x5b 0x31 0x31 0x38 0x3b 0x39 0x75";
const FIND_GLOBALLY_MENU_EQUIV: &str = "@~^f";
const PASTE_MENU_EQUIV: &str = "@~^v";

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

    let profile = default_profile_mut(dict)?;

    set_string(
        profile,
        "Normal Font",
        primary_font_value(primary_font_ps, size),
    );
    set_string(
        profile,
        "Non Ascii Font",
        primary_font_value(nonascii_font_ps, size),
    );
    profile.insert("Use Non-ASCII Font".into(), Value::Boolean(true));
    profile.insert("Non-ASCII Anti Aliased".into(), Value::Boolean(true));
    Ok(())
}

/// Apply the iTerm2-side pieces needed for Croft's macOS keyboard gestures:
/// free iTerm2 menu shortcuts that macOS consumes first, install the Search
/// shortcut globally, and bind Cmd+V in every profile to the CSI-u Cmd+V
/// sequence that Croft handles as Search paste.
pub fn apply_croft_key_settings(plist: &mut Value) -> Result<(), ITerm2Error> {
    let dict = plist
        .as_dictionary_mut()
        .ok_or(ITerm2Error::NotADictionary)?;

    let menu = dict_entry_mut(dict, "NSUserKeyEquivalents");
    set_string(
        menu,
        "Find Globally...",
        FIND_GLOBALLY_MENU_EQUIV.to_string(),
    );
    set_string(menu, "Paste", PASTE_MENU_EQUIV.to_string());

    let global = dict_entry_mut(dict, "GlobalKeyMap");
    global.insert(CMD_SHIFT_F_KEY.into(), send_hex_action(CMD_SHIFT_F_HEX, 0));
    global.insert(CMD_V_KEY.into(), send_hex_action(CMD_V_HEX, 0));

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;
    for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
        let profile_keys = dict_entry_mut(profile, "Keyboard Map");
        profile_keys.insert(CMD_V_KEY.into(), send_hex_action(CMD_V_HEX, 0));
    }

    Ok(())
}

fn set_string(dict: &mut Dictionary, key: &str, value: String) {
    dict.insert(key.into(), Value::String(value));
}

fn default_profile_mut(dict: &mut Dictionary) -> Result<&mut Dictionary, ITerm2Error> {
    let default_guid = dict
        .get("Default Bookmark Guid")
        .and_then(|v| v.as_string())
        .ok_or(ITerm2Error::NoDefaultGuid)?
        .to_string();

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;

    bookmarks
        .iter_mut()
        .filter_map(|v| v.as_dictionary_mut())
        .find(|d| d.get("Guid").and_then(|g| g.as_string()) == Some(&default_guid))
        .ok_or_else(|| ITerm2Error::NoMatchingProfile(default_guid))
}

fn dict_entry_mut<'a>(dict: &'a mut Dictionary, key: &str) -> &'a mut Dictionary {
    if !matches!(dict.get(key), Some(Value::Dictionary(_))) {
        dict.insert(key.into(), Value::Dictionary(Dictionary::new()));
    }
    dict.get_mut(key)
        .and_then(|v| v.as_dictionary_mut())
        .expect("dictionary value was just inserted")
}

fn send_hex_action(text: &str, apply_mode: i64) -> Value {
    let mut action = Dictionary::new();
    action.insert("Action".into(), Value::Integer(11.into()));
    action.insert("Apply Mode".into(), Value::Integer(apply_mode.into()));
    action.insert("Escaping".into(), Value::Integer(2.into()));
    action.insert("Text".into(), Value::String(text.to_string()));
    action.insert("Version".into(), Value::Integer(2.into()));
    Value::Dictionary(action)
}

/// Load → mutate → save the iTerm2 plist on disk.
#[cfg(test)]
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
        .to_file_binary(plist_path)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    Ok(())
}

/// Load → mutate → save every iTerm2 setting Croft needs: font fallback plus
/// Cmd+Shift+F and Search paste behavior.
pub fn install_croft_settings(
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
    apply_croft_key_settings(&mut value)?;
    value
        .to_file_binary(plist_path)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    Ok(())
}

pub fn default_plist_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
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

    fn dict_in<'a>(dict: &'a Dictionary, key: &str) -> &'a Dictionary {
        dict.get(key).unwrap().as_dictionary().unwrap()
    }

    fn action_text<'a>(dict: &'a Dictionary, key: &str) -> &'a str {
        dict.get(key)
            .unwrap()
            .as_dictionary()
            .unwrap()
            .get("Text")
            .unwrap()
            .as_string()
            .unwrap()
    }

    fn seed_stale_cmd_v_mappings(plist: &mut Value) {
        let top = plist.as_dictionary_mut().unwrap();
        dict_entry_mut(top, "GlobalKeyMap")
            .insert(CMD_V_KEY.into(), send_hex_action(CMD_V_HEX, 0));
        let bookmarks = top
            .get_mut("New Bookmarks")
            .unwrap()
            .as_array_mut()
            .unwrap();
        for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
            dict_entry_mut(profile, "Keyboard Map")
                .insert(CMD_V_KEY.into(), send_hex_action(CMD_V_HEX, 0));
        }
    }

    #[test]
    fn primary_font_value_concatenates_postscript_name_and_size() {
        assert_eq!(
            primary_font_value("MesloLGSNFM-Regular", 13),
            "MesloLGSNFM-Regular 13"
        );
        assert_eq!(
            primary_font_value("FiraCodeNFM-Reg", 14),
            "FiraCodeNFM-Reg 14"
        );
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
        assert_eq!(
            p.get("Use Non-ASCII Font").unwrap().as_boolean(),
            Some(true)
        );
    }

    #[test]
    fn apply_font_settings_only_touches_default_profile() {
        let mut plist = synth_plist("DEFAULT-GUID", &["OTHER-GUID", "DEFAULT-GUID"]);
        apply_font_settings(&mut plist, "F-Reg", "S-Reg", 13).unwrap();
        let other = profile_in(&plist, "OTHER-GUID");
        assert!(
            other.get("Normal Font").is_none(),
            "other profile should be untouched"
        );
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
        assert!(
            msg.contains("not found"),
            "expected 'not found' message, got: {msg}"
        );
    }

    #[test]
    fn apply_croft_key_settings_frees_find_and_paste_menu_shortcuts() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get("Find Globally...").and_then(|v| v.as_string()),
            Some(FIND_GLOBALLY_MENU_EQUIV)
        );
        assert_eq!(
            menu.get("Paste").and_then(|v| v.as_string()),
            Some(PASTE_MENU_EQUIV)
        );
    }

    #[test]
    fn apply_croft_key_settings_installs_global_search_and_cmd_v_mapping() {
        let mut plist = synth_plist("GUID-1", &["GUID-1", "GUID-2"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(action_text(global, CMD_SHIFT_F_KEY), CMD_SHIFT_F_HEX);
        assert_eq!(action_text(global, CMD_V_KEY), CMD_V_HEX);
        for guid in ["GUID-1", "GUID-2"] {
            let profile = profile_in(&plist, guid);
            let profile_keys = dict_in(profile, "Keyboard Map");
            assert_eq!(action_text(profile_keys, CMD_V_KEY), CMD_V_HEX);
        }
    }

    #[test]
    fn install_croft_settings_round_trips_fonts_and_keys_through_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        seed_stale_cmd_v_mappings(&mut plist);
        plist.to_file_xml(tmp.path()).unwrap();
        install_croft_settings(tmp.path(), "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let reloaded: Value = Value::from_file(tmp.path()).unwrap();
        let profile = profile_in(&reloaded, "GUID-1");
        assert_eq!(
            profile.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
        let top = reloaded.as_dictionary().unwrap();
        assert_eq!(
            action_text(dict_in(top, "GlobalKeyMap"), CMD_V_KEY),
            CMD_V_HEX
        );
    }
}
