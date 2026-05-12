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
/// `Cmd+Shift+Return`. Serialized identifier follows iTerm2's
/// iTermKeystroke format `0x<char>-0x<modifiers>-0x<virtualKeyCode>`:
/// character = 0xd (CR, the same unmodified value Return reports under
/// Shift), modifiers = 0x120000 (Cmd 0x100000 | Shift 0x20000),
/// virtualKeyCode = 0x24 (kVK_Return). Without this entry iTerm2 simply
/// drops the chord on the floor, which is why croft never saw it.
const CMD_SHIFT_ENTER_KEY: &str = "0xd-0x120000-0x24";
/// CSI-u sequence `ESC [ 13 ; 10 u` = Enter (codepoint 13) with kitty
/// modifier byte 10 (= 1 base + Shift(1) + Super(8)). Crossterm parses
/// this back into `KeyEvent { code: Enter, modifiers: SHIFT | SUPER }`,
/// which the Explorer's plain Enter handler already routes through
/// `tree.activate()` to toggle expand/collapse on folders.
const CMD_SHIFT_ENTER_HEX: &str = "0x1b 0x5b 0x31 0x33 0x3b 0x31 0x30 0x75";
const CMD_V_KEY: &str = "0x76-0x100000-0x9";
/// GlobalKeyMap keys + CSI-u payloads for the five Mac-style Cmd+letter
/// chords croft uses across panes (terminal copy / source-control
/// select-all / editor save / editor cut / editor undo). iTerm2's
/// `iTermApplication.sendEvent:` checks `GlobalKeyMap` ahead of the
/// NSResponder chain, so a forwarder here intercepts Cmd+letter before
/// AppKit's default copy:/cut:/selectAll:/undo: bindings consume it.
/// Each payload is `ESC [ <codepoint> ; 9 u`, where 9 = 1 base + Super(8)
/// in kitty's CSI-u modifier byte, which crossterm decodes into a
/// `KeyEvent { code: Char(letter), modifiers: SUPER }` that croft's
/// terminal/editor/tree handlers already accept.
const CMD_A_KEY: &str = "0x61-0x100000-0x0";
const CMD_A_HEX: &str = "0x1b 0x5b 0x39 0x37 0x3b 0x39 0x75";
const CMD_C_KEY: &str = "0x63-0x100000-0x8";
const CMD_C_HEX: &str = "0x1b 0x5b 0x39 0x39 0x3b 0x39 0x75";
const CMD_S_KEY: &str = "0x73-0x100000-0x1";
const CMD_S_HEX: &str = "0x1b 0x5b 0x31 0x31 0x35 0x3b 0x39 0x75";
const CMD_X_KEY: &str = "0x78-0x100000-0x7";
const CMD_X_HEX: &str = "0x1b 0x5b 0x31 0x32 0x30 0x3b 0x39 0x75";
const CMD_Z_KEY: &str = "0x7a-0x100000-0x6";
const CMD_Z_HEX: &str = "0x1b 0x5b 0x31 0x32 0x32 0x3b 0x39 0x75";
/// Vim-style chord starts and goto-bottom that the editor consumes.
/// CSI-u `ESC [ <codepoint> ; 9 u` for Cmd+letter and
/// `ESC [ <shifted-glyph> ; 10 u` for Cmd+Shift+letter; modifier byte
/// 9 = 1 base + Super(8), 10 adds Shift(1).
const CMD_D_KEY: &str = "0x64-0x100000-0x2";
const CMD_D_HEX: &str = "0x1b 0x5b 0x31 0x30 0x30 0x3b 0x39 0x75";
const CMD_G_KEY: &str = "0x67-0x100000-0x5";
const CMD_G_HEX: &str = "0x1b 0x5b 0x31 0x30 0x33 0x3b 0x39 0x75";
const CMD_Y_KEY: &str = "0x79-0x100000-0x10";
const CMD_Y_HEX: &str = "0x1b 0x5b 0x31 0x32 0x31 0x3b 0x39 0x75";
const CMD_O_KEY: &str = "0x6f-0x100000-0x1f";
const CMD_O_HEX: &str = "0x1b 0x5b 0x31 0x31 0x31 0x3b 0x39 0x75";
const CMD_SHIFT_G_KEY: &str = "0x47-0x120000-0x5";
const CMD_SHIFT_G_HEX: &str = "0x1b 0x5b 0x37 0x31 0x3b 0x31 0x30 0x75";
const CMD_SHIFT_O_KEY: &str = "0x4f-0x120000-0x1f";
const CMD_SHIFT_O_HEX: &str = "0x1b 0x5b 0x37 0x39 0x3b 0x31 0x30 0x75";
/// Cmd+0..Cmd+9 forward as CSI-u so the editor's vim chord can use them
/// as count digits (e.g. `Cmd+5 Cmd+g g` jumps to line 5). Without these,
/// iTerm2 catches Cmd+digit for its own "Select Tab N" action and croft
/// never sees the keystroke. Mac virtual key codes for the number row
/// are non-contiguous: `kVK_ANSI_1=0x12 … kVK_ANSI_5=0x17`, with 0 last
/// at `kVK_ANSI_0=0x1d`.
const CMD_DIGIT_CHORDS: &[(&str, &str)] = &[
    ("0x30-0x100000-0x1d", "0x1b 0x5b 0x34 0x38 0x3b 0x39 0x75"),
    ("0x31-0x100000-0x12", "0x1b 0x5b 0x34 0x39 0x3b 0x39 0x75"),
    ("0x32-0x100000-0x13", "0x1b 0x5b 0x35 0x30 0x3b 0x39 0x75"),
    ("0x33-0x100000-0x14", "0x1b 0x5b 0x35 0x31 0x3b 0x39 0x75"),
    ("0x34-0x100000-0x15", "0x1b 0x5b 0x35 0x32 0x3b 0x39 0x75"),
    ("0x35-0x100000-0x17", "0x1b 0x5b 0x35 0x33 0x3b 0x39 0x75"),
    ("0x36-0x100000-0x16", "0x1b 0x5b 0x35 0x34 0x3b 0x39 0x75"),
    ("0x37-0x100000-0x1a", "0x1b 0x5b 0x35 0x35 0x3b 0x39 0x75"),
    ("0x38-0x100000-0x1c", "0x1b 0x5b 0x35 0x36 0x3b 0x39 0x75"),
    ("0x39-0x100000-0x19", "0x1b 0x5b 0x35 0x37 0x3b 0x39 0x75"),
];
/// Top-level plist key that disables iTerm2's mouse-reporting-frustration
/// banner. Backed by iTermAdvancedSettingsModel's
/// `noSyncNeverAskAboutMouseReportingFrustration` property whose plist
/// storage key is PascalCase per `DEFINE_SETTABLE_BOOL`.
const MOUSE_REPORTING_FRUSTRATION_KEY: &str = "NoSyncNeverAskAboutMouseReportingFrustration";
/// `Cmd+Shift+/`. Character is the *shifted* glyph `?` (0x3f), modifiers
/// are Cmd+Shift (0x120000), virtualKeyCode is `kVK_ANSI_Slash` (0x2c).
/// macOS reserves this chord for the Help-menu Search field (Apple
/// writes it as Cmd+?). The `NSUserKeyEquivalents` override below
/// repoints "Show Help Menu" away from Cmd+?, freeing the chord so this
/// GlobalKeyMap forwarder can fire.
const CMD_SHIFT_SLASH_KEY: &str = "0x3f-0x120000-0x2c";
/// CSI-u sequence `ESC [ 63 ; 10 u` = '?' (codepoint 63) with kitty
/// modifier byte 10 (= 1 base + Shift(1) + Super(8)). Crossterm decodes
/// this back to `KeyEvent { code: Char('?'), modifiers: SHIFT | SUPER }`,
/// which `is_tree_make_parent_root_key` accepts via its `Char('?')`
/// branch.
const CMD_SHIFT_SLASH_HEX: &str = "0x1b 0x5b 0x36 0x33 0x3b 0x31 0x30 0x75";
const FIND_GLOBALLY_MENU_EQUIV: &str = "@~^f";
const FIND_MENU_EQUIV: &str = "@~f";
/// macOS calls the Help-menu Cmd+? Search shortcut "Show Help Menu" in
/// its keyboard-shortcuts UI; that is the menu-item title NSUserKeyEquivalents
/// recognizes. Setting it here in iTerm2's plist overrides the system
/// binding for iTerm2 only.
const HELP_MENU_KEY: &str = "Show Help Menu";
/// Cmd+Opt+? (i.e., Cmd+Opt+Shift+/). Picked because it is unbound by
/// default on macOS and Opt being held neutralizes croft's Explorer
/// predicate (which rejects ALT), so the chord can no longer fall back
/// into croft's parent-folder action either.
const HELP_MENU_EQUIV: &str = "@~?";

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
    // Reclaim Cmd+Shift+/ from the macOS Help-menu Search field.
    // AppKit binds Cmd+? to the Help menu at the app level, ahead of
    // iTerm2's GlobalKeyMap; without this override the chord opens
    // Help instead of reaching croft. Pointing "Show Help Menu" at
    // Cmd+Opt+? leaves Help reachable on a chord croft does not use.
    set_string(menu, HELP_MENU_KEY, HELP_MENU_EQUIV.to_string());
    // Relocate iTerm2's Edit menu items off the standard Cmd+letter
    // shortcuts so croft's terminal-pane Cmd+C (copy via OSC 52),
    // editor Cmd+S / Cmd+X / Cmd+Z, and Source Control / editor Cmd+A
    // can all reach their handlers. Without this, even after stripping
    // the profile-level Send-Hex bindings below, AppKit's standard
    // Edit menu would still claim the chord at the menu layer.
    set_string(menu, "Copy", "@~c".to_string());
    set_string(menu, "Cut", "@~x".to_string());
    set_string(menu, "Select All", "@~a".to_string());
    set_string(menu, "Undo", "@~z".to_string());
    menu.remove("Paste");
    // Relocate iTerm2 menu items that would otherwise catch the editor's
    // vim chords at the menu-bar layer. Each is moved to Cmd+Opt+<key>
    // so the original action stays reachable, but the bare Cmd+<key>
    // chord is freed for the GlobalKeyMap forwarder below.
    set_string(menu, "Split Vertically with Same Profile", "@~d".to_string());
    set_string(menu, "Find Next", "@~g".to_string());
    set_string(menu, "Find Previous", "@~G".to_string());
    set_string(menu, "Jump to Selection", "@~y".to_string());
    // iTerm2's Window menu binds Cmd+1..Cmd+9 to Select Tab. Move each
    // to Cmd+Opt+digit so croft can capture Cmd+digit as a vim count.
    for (i, label) in [
        "Select Tab 1",
        "Select Tab 2",
        "Select Tab 3",
        "Select Tab 4",
        "Select Tab 5",
        "Select Tab 6",
        "Select Tab 7",
        "Select Tab 8",
        "Select Tab 9",
    ]
    .iter()
    .enumerate()
    {
        set_string(menu, label, format!("@~{}", i + 1));
    }

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
    global.insert(
        CMD_SHIFT_SLASH_KEY.into(),
        send_hex_action(CMD_SHIFT_SLASH_HEX, 0),
    );
    global.insert(
        CMD_SHIFT_ENTER_KEY.into(),
        send_hex_action(CMD_SHIFT_ENTER_HEX, 0),
    );
    // Mac-style Cmd+letter chords: forward each as a CSI-u sequence so
    // AppKit's NSResponder defaults (copy: / cut: / selectAll: / undo:)
    // don't consume them at the textview layer. Without these, even
    // with the Edit menu items relocated via NSUserKeyEquivalents above,
    // Cmd+C still never reaches croft because PTYTextView still answers
    // copy: from the default key bindings dictionary.
    for (key, hex) in [
        (CMD_A_KEY, CMD_A_HEX),
        (CMD_C_KEY, CMD_C_HEX),
        (CMD_S_KEY, CMD_S_HEX),
        (CMD_X_KEY, CMD_X_HEX),
        (CMD_Z_KEY, CMD_Z_HEX),
        (CMD_D_KEY, CMD_D_HEX),
        (CMD_G_KEY, CMD_G_HEX),
        (CMD_Y_KEY, CMD_Y_HEX),
        (CMD_O_KEY, CMD_O_HEX),
        (CMD_SHIFT_G_KEY, CMD_SHIFT_G_HEX),
        (CMD_SHIFT_O_KEY, CMD_SHIFT_O_HEX),
    ] {
        global.insert(key.into(), send_hex_action(hex, 0));
    }
    for (key, hex) in CMD_DIGIT_CHORDS {
        global.insert((*key).into(), send_hex_action(hex, 0));
    }
    global.remove(CMD_V_KEY);

    dict.insert(
        MOUSE_REPORTING_FRUSTRATION_KEY.into(),
        Value::Boolean(true),
    );

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
    fn apply_croft_key_settings_forwards_cmd_letter_chords_as_csi_u_so_iterm_responder_chain_does_not_consume_them() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (CMD_A_KEY, CMD_A_HEX, "Cmd+A (select all / multi-select)"),
            (CMD_C_KEY, CMD_C_HEX, "Cmd+C (copy via OSC 52)"),
            (CMD_S_KEY, CMD_S_HEX, "Cmd+S (editor save / source control stage)"),
            (CMD_X_KEY, CMD_X_HEX, "Cmd+X (editor cut)"),
            (CMD_Z_KEY, CMD_Z_HEX, "Cmd+Z (editor undo)"),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap is missing the CSI-u forwarder for {label}; without it, AppKit's NSResponder default key bindings catch the chord (Cmd+C -> copy: on PTYTextView) before croft's terminal handler sees a Char-with-Super key event, which is why Cmd+C silently fails to copy the croft selection even with the NSUserKeyEquivalents Edit-menu relocations in place"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_editor_vim_chord_starts_and_count_digits() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (CMD_D_KEY, CMD_D_HEX, "Cmd+D (vim dd chord start)"),
            (CMD_G_KEY, CMD_G_HEX, "Cmd+G (vim gg chord start)"),
            (CMD_Y_KEY, CMD_Y_HEX, "Cmd+Y (vim yy chord start)"),
            (CMD_O_KEY, CMD_O_HEX, "Cmd+O (open line below)"),
            (CMD_SHIFT_G_KEY, CMD_SHIFT_G_HEX, "Cmd+Shift+G (goto bottom)"),
            (CMD_SHIFT_O_KEY, CMD_SHIFT_O_HEX, "Cmd+Shift+O (open line above)"),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap missing CSI-u forwarder for {label}; without it, iTerm2 swallows the chord at the menu/responder layer (e.g. Cmd+D = Split Pane) and the editor's chord state never advances"
            );
        }
        for (key, hex) in CMD_DIGIT_CHORDS {
            assert_eq!(
                action_text(global, key),
                *hex,
                "GlobalKeyMap missing CSI-u forwarder for Cmd+digit {key}; without it, iTerm2 catches Cmd+digit for Select Tab N and the editor's vim count chord cannot start with a leading digit"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_relocates_iterm_menu_items_off_vim_chord_letters() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        for (item, expected) in [
            ("Split Vertically with Same Profile", "@~d"),
            ("Find Next", "@~g"),
            ("Find Previous", "@~G"),
            ("Select Tab 1", "@~1"),
            ("Select Tab 9", "@~9"),
        ] {
            assert_eq!(
                menu.get(item).and_then(|v| v.as_string()),
                Some(expected),
                "iTerm2 menu item {item} must be relocated off its default Cmd-chord, otherwise the menu bar catches the chord before the GlobalKeyMap forwarder fires"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_silences_iterm_mouse_reporting_frustration_banner() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        assert_eq!(
            top.get(MOUSE_REPORTING_FRUSTRATION_KEY).and_then(|v| v.as_boolean()),
            Some(true),
            "iTerm2's iTermMouseReportingFrustrationDetector watches raw Cmd+C keyDown and pops the 'Looks like you're trying to copy to the pasteboard...' banner whenever mouse reporting is on and iTerm2 has no selection (which is the steady state under croft, since croft owns the mouse). The advanced setting NoSyncNeverAskAboutMouseReportingFrustration suppresses that detector entirely."
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_edit_menu_items_off_cmd_letter_so_iterm_does_not_steal_them_back() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        for (item, expected) in [
            ("Copy", "@~c"),
            ("Cut", "@~x"),
            ("Select All", "@~a"),
            ("Undo", "@~z"),
        ] {
            assert_eq!(
                menu.get(item).and_then(|v| v.as_string()),
                Some(expected),
                "iTerm2's Edit > {item} menu item must be relocated off its default Cmd-letter shortcut, otherwise once the profile-level Send-Hex bindings are stripped the menu shortcut would still claim the chord and croft would never see the keystroke"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_relocates_help_menu_off_cmd_shift_slash() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get(HELP_MENU_KEY).and_then(|v| v.as_string()),
            Some(HELP_MENU_EQUIV),
            "Cmd+Shift+/ is reserved by macOS as Cmd+? for the Help menu's Search field; AppKit captures the chord at the app level before iTerm2's GlobalKeyMap is consulted. Re-pointing the 'Show Help Menu' NSUserKeyEquivalents item at Cmd+Opt+? frees the chord so the GlobalKeyMap CSI-u forwarder below can forward it to croft."
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_slash_as_csi_u_so_explorer_make_parent_root_fires() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_SLASH_KEY),
            CMD_SHIFT_SLASH_HEX,
            "GlobalKeyMap must forward Cmd+Shift+/ as a CSI-u sequence so croft's `is_tree_make_parent_root_key` predicate fires from the Explorer pane. Encoding: '?' (shifted '/', codepoint 0x3f = 63) with kitty modifier byte 10 = 1+Shift(1)+Super(8), giving `ESC [ 63 ; 10 u`."
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_enter_as_csi_u_so_iterm_does_not_swallow_it() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_ENTER_KEY),
            CMD_SHIFT_ENTER_HEX,
            "Cmd+Shift+Return must be hex-bound at the iTerm2 level: with no binding, iTerm2 never forwards the chord to the TUI, so croft never sees the keystroke. The CSI-u payload encodes Enter (codepoint 13) with kitty modifier byte 10 = 1+Shift(1)+Super(8), which crossterm decodes back to KeyEvent {{ code: Enter, modifiers: SHIFT|SUPER }}."
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
