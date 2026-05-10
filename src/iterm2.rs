use anyhow::{Context, Result};
use plist::{Dictionary, Value};
use std::path::{Path, PathBuf};

pub const ITERM2_PLIST_REL: &str = "Library/Preferences/com.googlecode.iterm2.plist";
const CMD_SHIFT_F_KEY: &str = "0x46-0x120000-0x3";
const CMD_SHIFT_F_HEX: &str = "0x1b 0x5b 0x37 0x30 0x3b 0x31 0x30 0x75";
const CMD_F_KEY: &str = "0x66-0x100000-0x3";
const CMD_F_HEX: &str = "0x1b 0x5b 0x31 0x30 0x32 0x3b 0x39 0x75";
const CMD_R_KEY: &str = "0x72-0x100000-0xf";
const CMD_R_HEX: &str = "0x1b 0x5b 0x31 0x31 0x34 0x3b 0x39 0x75";
const CMD_SLASH_KEY: &str = "0x2f-0x100000-0x2c";
const CMD_SLASH_HEX: &str = "0x1b 0x5b 0x34 0x37 0x3b 0x39 0x75";
const CMD_V_KEY: &str = "0x76-0x100000-0x9";
const FIND_GLOBALLY_MENU_EQUIV: &str = "@~^f";
const FIND_MENU_EQUIV: &str = "@~f";

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

/// Apply the iTerm2-side pieces needed for Croft's macOS keyboard gestures.
/// Installs the Cmd+Shift+F search shortcut globally, frees the matching
/// menu equivalent so macOS doesn't eat it for "Find Globally...", and
/// scrubs any legacy Cmd+V or Paste-menu remappings that older croft
/// versions wrote in. Cmd+V is intentionally **not** bound: leaving it on
/// the default Edit menu shortcut routes through iTerm2's native Paste
/// action, which emits a bracketed-paste sequence carrying the local
/// clipboard. That works identically in local and SSH'd croft sessions
/// (croft handles `Event::Paste`); intercepting Cmd+V as a key event
/// instead — the previous design — broke paste over SSH because the
/// remote process has no path to the local Mac clipboard.
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
    // Relocate iTerm's "Find" menu item off Cmd+F so the GlobalKeyMap
    // binding below wins reliably. Users that still want iTerm's
    // in-pane find can use Cmd+Opt+F.
    set_string(menu, "Find...", FIND_MENU_EQUIV.to_string());
    menu.remove("Paste");

    let global = dict_entry_mut(dict, "GlobalKeyMap");
    global.insert(CMD_SHIFT_F_KEY.into(), send_hex_action(CMD_SHIFT_F_HEX, 0));
    // Explorer shortcuts: forward Cmd+F / Cmd+R / Cmd+/ to croft as
    // CSI-u sequences. Croft handles them only when the Explorer pane
    // is focused; elsewhere the keys are passed through as raw input,
    // which means giving up iTerm's own actions on those chords
    // (Find / Clear Buffer) while croft is running. The user agreed.
    global.insert(CMD_F_KEY.into(), send_hex_action(CMD_F_HEX, 0));
    global.insert(CMD_R_KEY.into(), send_hex_action(CMD_R_HEX, 0));
    global.insert(CMD_SLASH_KEY.into(), send_hex_action(CMD_SLASH_HEX, 0));
    global.remove(CMD_V_KEY);

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;
    for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
        if let Some(Value::Dictionary(profile_keys)) = profile.get_mut("Keyboard Map") {
            profile_keys.remove(CMD_V_KEY);
        }
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

    /// The historical kitty CSI-u Cmd+V escape that older croft versions
    /// installed in iTerm2 plists. Tests use it to seed a "legacy state"
    /// fixture so we can prove `apply_croft_key_settings` cleans it up.
    const LEGACY_CMD_V_HEX: &str = "0x1b 0x5b 0x31 0x31 0x38 0x3b 0x39 0x75";

    fn seed_stale_cmd_v_mappings(plist: &mut Value) {
        let top = plist.as_dictionary_mut().unwrap();
        dict_entry_mut(top, "GlobalKeyMap")
            .insert(CMD_V_KEY.into(), send_hex_action(LEGACY_CMD_V_HEX, 0));
        let bookmarks = top
            .get_mut("New Bookmarks")
            .unwrap()
            .as_array_mut()
            .unwrap();
        for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
            dict_entry_mut(profile, "Keyboard Map")
                .insert(CMD_V_KEY.into(), send_hex_action(LEGACY_CMD_V_HEX, 0));
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
    fn apply_croft_key_settings_frees_find_menu_shortcut_only() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get("Find Globally...").and_then(|v| v.as_string()),
            Some(FIND_GLOBALLY_MENU_EQUIV)
        );
        assert!(
            menu.get("Paste").is_none(),
            "Paste must remain on its default Cmd+V menu shortcut so iTerm2 fires its native bracketed-paste action when Cmd+V is pressed; remapping it off Cmd+V breaks paste over SSH because the resulting key event has no clipboard reachable from the remote process"
        );
    }

    #[test]
    fn apply_croft_key_settings_installs_global_search_only() {
        let mut plist = synth_plist("GUID-1", &["GUID-1", "GUID-2"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(action_text(global, CMD_SHIFT_F_KEY), CMD_SHIFT_F_HEX);
        assert!(
            global.get(CMD_V_KEY).is_none(),
            "Cmd+V must not be hex-bound at the iTerm2 level; intercepting it as a key event prevents the terminal's native paste from emitting a bracketed-paste sequence, which is the only clipboard path that works over SSH"
        );
        for guid in ["GUID-1", "GUID-2"] {
            let profile = profile_in(&plist, guid);
            let cmd_v_in_profile = profile
                .get("Keyboard Map")
                .and_then(|v| v.as_dictionary())
                .and_then(|d| d.get(CMD_V_KEY));
            assert!(
                cmd_v_in_profile.is_none(),
                "profile-level Cmd+V binding must not exist (whether the Keyboard Map dict is absent or just missing this key) so every profile defers to iTerm2's native paste action"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_clears_legacy_cmd_v_bindings() {
        let mut plist = synth_plist("GUID-1", &["GUID-1", "GUID-2"]);
        seed_stale_cmd_v_mappings(&mut plist);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert!(
            global.get(CMD_V_KEY).is_none(),
            "re-running setup must remove the legacy GlobalKeyMap Cmd+V hex binding installed by older croft versions"
        );
        for guid in ["GUID-1", "GUID-2"] {
            let profile = profile_in(&plist, guid);
            let profile_keys = dict_in(profile, "Keyboard Map");
            assert!(
                profile_keys.get(CMD_V_KEY).is_none(),
                "re-running setup must remove the legacy profile-level Cmd+V hex binding"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_clears_legacy_paste_menu_remap() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        {
            let top = plist.as_dictionary_mut().unwrap();
            let menu = dict_entry_mut(top, "NSUserKeyEquivalents");
            set_string(menu, "Paste", "@~^v".to_string());
        }
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert!(
            menu.get("Paste").is_none(),
            "re-running setup must remove the legacy Paste -> Cmd+Opt+Ctrl+V menu remap that older croft versions installed; the menu must fall back to the default Cmd+V shortcut so the native paste action fires"
        );
    }

    #[test]
    fn install_croft_settings_round_trips_fonts_and_clears_cmd_v_through_disk() {
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
        assert!(
            dict_in(top, "GlobalKeyMap").get(CMD_V_KEY).is_none(),
            "round-trip: Cmd+V binding must not survive on disk after a fresh setup"
        );
        let profile_keys = dict_in(profile, "Keyboard Map");
        assert!(
            profile_keys.get(CMD_V_KEY).is_none(),
            "round-trip: profile-level Cmd+V binding must not survive on disk after a fresh setup"
        );
    }
}
