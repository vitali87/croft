//! User-defined key bindings, loaded from `~/.config/croft/keybindings.json`.
//!
//! croft's default chords are hand-wired in the app key router; this layer sits
//! *in front* of them so a VS Code refugee can carry their muscle memory over.
//! Each binding maps a chord string (`"ctrl+shift+p"`, `"cmd+,"`, `"f2"`) to a
//! palette [`Command`] id (the stable snake_case name from
//! [`Command::id`](crate::widgets::command_palette::Command::id)). A bound chord
//! wins over the built-in handler for the same chord; an unbound chord falls
//! through untouched.
//!
//! JSON shape mirrors VS Code's `keybindings.json`:
//! ```json
//! [
//!   { "key": "cmd+,",        "command": "open_settings" },
//!   { "key": "ctrl+shift+p", "command": "quick_open" }
//! ]
//! ```
//!
//! ponytail: no `when` clauses. The app only consults the keymap when the
//! terminal pane is not focused and the chord carries a real modifier (or is a
//! function key), which stands in for VS Code's `editorTextFocus`-style context
//! and keeps plain typing and raw terminal control keys untouched. Add real
//! `when` contexts only if a user actually needs finer scoping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::widgets::command_palette::Command;

/// A normalized key chord: a base key plus the four modifier flags. Both the
/// parsed config side and the live-event side run through [`Chord::normalize`]
/// so the two always compare on the same footing (letter case folded into an
/// explicit `SHIFT`, only the four real modifiers retained).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Chord {
    /// Fold a raw `(code, mods)` into the canonical form used for comparison:
    /// lower-case letters (with the uppercasing re-expressed as `SHIFT`) and
    /// only the CONTROL/ALT/SHIFT/SUPER bits kept.
    fn normalize(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut m = mods
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
        let code = match code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                m |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        Self { code, mods: m }
    }

    /// Build the chord an incoming event represents.
    pub fn from_event(key: KeyEvent) -> Self {
        Self::normalize(key.code, key.modifiers)
    }

    /// The iTerm2 `GlobalKeyMap` forwarder for this chord as `(key, csi_u_hex)`,
    /// or `None` when no forwarding is needed or the chord can't be encoded.
    ///
    /// macOS reserves Cmd for its menus, so only Super chords are swallowed
    /// before reaching croft; Ctrl/Alt/function chords already arrive, so those
    /// return `None`. Forwarding re-emits the chord as the same CSI-u sequence
    /// the built-in chords use, which crossterm decodes straight back into the
    /// `KeyEvent` the keymap matches. The encoding reproduces the hand-written
    /// forwarders in `iterm2.rs` byte-for-byte (asserted by a round-trip test).
    ///
    /// ponytail: only `Char` keys, and shift only with letters — a shifted
    /// non-letter's glyph (Shift+/ = ?) is keyboard-layout dependent. Cmd chords
    /// on named/function keys are niche; add them if a user needs one.
    pub fn iterm2_forwarder(self) -> Option<(String, String)> {
        if !self.mods.contains(KeyModifiers::SUPER) {
            return None;
        }
        let KeyCode::Char(c) = self.code else {
            return None;
        };
        let shift = self.mods.contains(KeyModifiers::SHIFT);
        if shift && !c.is_ascii_alphabetic() {
            return None;
        }
        let vk = mac_ansi_keycode(c)?;
        let codepoint = if shift {
            c.to_ascii_uppercase() as u32
        } else {
            c as u32
        };
        let mut mask = 0x10_0000u32; // Cmd
        let mut modbyte = 1u32 + 8; // CSI-u base (1) + Super (8)
        if shift {
            mask |= 0x2_0000;
            modbyte += 1;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            mask |= 0x8_0000;
            modbyte += 2;
        }
        if self.mods.contains(KeyModifiers::CONTROL) {
            mask |= 0x4_0000;
            modbyte += 4;
        }
        Some((
            format!("0x{codepoint:x}-0x{mask:x}-0x{vk:x}"),
            csi_u_hex(codepoint, modbyte),
        ))
    }

    /// The Ghostty keybind trigger string for this chord (e.g. `"cmd+shift+p"`),
    /// or `None` under the same conditions as [`Self::iterm2_forwarder`]. Ghostty
    /// re-emits the chord via its `csi:` action using that forwarder's payload,
    /// so the two must gate identically.
    pub fn ghostty_trigger(self) -> Option<String> {
        if !self.mods.contains(KeyModifiers::SUPER) {
            return None;
        }
        let KeyCode::Char(c) = self.code else {
            return None;
        };
        if self.mods.contains(KeyModifiers::SHIFT) && !c.is_ascii_alphabetic() {
            return None;
        }
        let mut t = String::from("cmd");
        if self.mods.contains(KeyModifiers::CONTROL) {
            t.push_str("+ctrl");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            t.push_str("+alt");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            t.push_str("+shift");
        }
        t.push('+');
        t.push(c.to_ascii_lowercase());
        Some(t)
    }

    /// Parse a config string like `"cmd+shift+p"`, `"ctrl+/"`, `"f2"`. Returns
    /// `None` on an unrecognized token so one bad line is skipped, not fatal.
    pub fn parse(s: &str) -> Option<Chord> {
        if s.trim().is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::empty();
        let mut code: Option<KeyCode> = None;
        for raw in s.split('+') {
            let tok = raw.trim().to_ascii_lowercase();
            if tok.is_empty() {
                // A literal "+" key: the split produced an empty token because
                // the key itself is '+'. Treat it as the base key.
                code = Some(KeyCode::Char('+'));
                continue;
            }
            match tok.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "opt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "cmd" | "command" | "super" | "meta" | "win" => mods |= KeyModifiers::SUPER,
                // "mod" is the portable modifier: Cmd on macOS, Ctrl elsewhere,
                // so one config drives both the local Mac and remote Linux
                // builds (the golden rule: identical behavior on both).
                "mod" => {
                    if cfg!(target_os = "macos") {
                        mods |= KeyModifiers::SUPER;
                    } else {
                        mods |= KeyModifiers::CONTROL;
                    }
                }
                other => code = Some(parse_key(other)?),
            }
        }
        code.map(|c| Chord::normalize(c, mods))
    }
}

/// The CSI-u escape `ESC [ <codepoint> ; <modbyte> u` as the space-separated
/// hex-byte string iTerm2's "Send Hex Code" action wants (each decimal digit
/// becomes its ASCII byte).
fn csi_u_hex(codepoint: u32, modbyte: u32) -> String {
    let mut parts = vec![String::from("0x1b"), String::from("0x5b")];
    parts.extend(codepoint.to_string().bytes().map(|b| format!("0x{b:x}")));
    parts.push(String::from("0x3b"));
    parts.extend(modbyte.to_string().bytes().map(|b| format!("0x{b:x}")));
    parts.push(String::from("0x75"));
    parts.join(" ")
}

/// The macOS ANSI virtual key code (`kVK_ANSI_*` from Carbon HIToolbox) for a
/// US-layout character. iTerm2's `GlobalKeyMap` key is `codepoint-mask-vkcode`,
/// so a forwarder can't be built without this. `None` for a key not on the
/// table (skip its forwarder rather than emit a wrong one).
fn mac_ansi_keycode(c: char) -> Option<u32> {
    Some(match c.to_ascii_lowercase() {
        'a' => 0x00,
        'b' => 0x0b,
        'c' => 0x08,
        'd' => 0x02,
        'e' => 0x0e,
        'f' => 0x03,
        'g' => 0x05,
        'h' => 0x04,
        'i' => 0x22,
        'j' => 0x26,
        'k' => 0x28,
        'l' => 0x25,
        'm' => 0x2e,
        'n' => 0x2d,
        'o' => 0x1f,
        'p' => 0x23,
        'q' => 0x0c,
        'r' => 0x0f,
        's' => 0x01,
        't' => 0x11,
        'u' => 0x20,
        'v' => 0x09,
        'w' => 0x0d,
        'x' => 0x07,
        'y' => 0x10,
        'z' => 0x06,
        '0' => 0x1d,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1a,
        '8' => 0x1c,
        '9' => 0x19,
        '-' => 0x1b,
        '=' => 0x18,
        '[' => 0x21,
        ']' => 0x1e,
        '\\' => 0x2a,
        ';' => 0x29,
        '\'' => 0x27,
        ',' => 0x2b,
        '.' => 0x2f,
        '/' => 0x2c,
        '`' => 0x32,
        ' ' => 0x31,
        _ => return None,
    })
}

fn parse_key(tok: &str) -> Option<KeyCode> {
    if let Some(n) = tok.strip_prefix('f')
        && let Ok(num) = n.parse::<u8>()
        && (1..=24).contains(&num)
    {
        return Some(KeyCode::F(num));
    }
    Some(match tok {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        s => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char token that is not a named key
            }
            KeyCode::Char(c)
        }
    })
}

/// Strip `//` line comments so the JSONC config (and the seeded template) parse
/// through `serde_json`, which rejects comments. Tracks string state so a `//`
/// inside a quoted value survives. ponytail: only `//` line comments, no `/* */`
/// blocks — the config has never needed them; add if a user asks.
pub(crate) fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// One `{ "key": ..., "command": ... }` row as written on disk.
#[derive(Debug, Deserialize)]
struct Binding {
    key: String,
    command: String,
}

/// The resolved user keymap: a chord lookup table into [`Command`].
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<Chord, Command>,
}

impl Keymap {
    /// Load and resolve `keybindings.json`, ignoring unparsable chords and
    /// unknown command ids (a typo skips that one line, never blocks startup).
    /// A missing file yields an empty keymap.
    pub fn load(path: &Path) -> Self {
        let Ok(json) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let (map, warnings) = Self::resolve(&json);
        // Reported where every other refusal is reported, so a keybinding that
        // silently did nothing is findable in the same place the user already
        // looks (#315). Only on the real load path: `from_json` is also the
        // test and reload entry point, and neither should write to a global.
        for warning in warnings {
            crate::output::push(
                "Keybindings",
                crate::output::OutputLevel::Warn,
                &format!("{}: {warning}", path.display()),
            );
        }
        map
    }

    /// The keymap alone, discarding what [`resolve`](Self::resolve) had to say
    /// about the file. Test-only: the real load path reports those warnings
    /// rather than dropping them.
    #[cfg(test)]
    pub fn from_json(json: &str) -> Self {
        let (map, _) = Self::resolve(json);
        map
    }

    /// [`from_json`](Self::from_json), also returning what it had to say about
    /// the file. Split so the reporting can be tested without reading a global
    /// output buffer, and so `from_json` stays the one-line caller API.
    ///
    /// A row that does not do what it says must say so (#315). Three ways a
    /// row can fail to bind, all of them previously silent:
    ///
    /// * the chord does not parse,
    /// * the command id is not one croft has,
    /// * an earlier row already bound that chord.
    ///
    /// The last one keeps VS Code's semantics - a later row deliberately
    /// overrides an earlier one, which is how a user rebinds a default - so
    /// this reports the override rather than refusing it. What it must not do
    /// is stay quiet: a user who binds one chord twice by accident otherwise
    /// gets the second binding with nothing anywhere to say the first was
    /// discarded.
    pub(crate) fn resolve(json: &str) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        // A malformed file is the worst of the silent cases and the likeliest:
        // this is hand-edited JSON with no schema, and one trailing comma cost
        // the user EVERY binding with nothing anywhere to say so. Reported
        // rather than swallowed - an empty file legitimately means no
        // bindings, but a broken one means bindings the user wrote that
        // vanished, and the two must not look alike.
        let rows: Vec<Binding> = match serde_json::from_str(&strip_line_comments(json)) {
            Ok(rows) => rows,
            Err(e) => {
                warnings.push(format!(
                    "the file does not parse ({e}); NO keybindings were loaded from it"
                ));
                Vec::new()
            }
        };
        let mut bindings: HashMap<Chord, Command> = HashMap::new();
        let mut sources: HashMap<Chord, String> = HashMap::new();
        for row in rows {
            let Some(chord) = Chord::parse(&row.key) else {
                warnings.push(format!(
                    "`{}` is not a chord croft can parse; the row binding `{}` was skipped",
                    row.key, row.command
                ));
                continue;
            };
            let Some(cmd) = Command::from_id(&row.command) else {
                warnings.push(format!(
                    "`{}` is not a command croft has; the row for `{}` was skipped",
                    row.command, row.key
                ));
                continue;
            };
            if let Some(previous) = sources.insert(chord, row.command.clone()) {
                warnings.push(format!(
                    "`{}` is bound more than once; the later row (`{}`) wins and the earlier one (`{}`) was discarded",
                    row.key, row.command, previous
                ));
            }
            bindings.insert(chord, cmd);
        }
        (Self { bindings }, warnings)
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Every bound chord, for the terminal-setup pass that installs forwarders.
    pub fn chords(&self) -> Vec<Chord> {
        self.bindings.keys().copied().collect()
    }

    /// The command a live key event is bound to, if any. Bare keys and
    /// modifier-less letters are never matched here (see the module note): the
    /// caller only asks for chords that carry a modifier or are function keys.
    pub fn command_for(&self, key: KeyEvent) -> Option<Command> {
        self.bindings.get(&Chord::from_event(key)).copied()
    }
}

pub fn keybindings_path() -> PathBuf {
    crate::prefs::config_dir().join("keybindings.json")
}

/// The starter file written on first "Open Keyboard Shortcuts (JSON)" so the
/// user lands on a working example rather than a blank buffer. Uses `mod` so the
/// same file behaves the same on macOS and Linux.
pub const TEMPLATE: &str = r#"// croft keyboard shortcuts. Rebind any palette command by its id.
// "mod" is Cmd on macOS, Ctrl on Linux. Modifiers: ctrl, alt/opt, shift, cmd/super, mod.
// Run "Preferences: Open Settings" to see command ids, or the command palette.
//
// In iTerm2, reserved Cmd chords must be forwarded first: run `croft setup-iterm2`.
// Ctrl / Alt / function-key bindings always reach croft.
[
  { "key": "mod+,", "command": "open_settings" }
]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_modifier_stack_and_letter() {
        let c = Chord::parse("ctrl+shift+p").unwrap();
        assert_eq!(
            c,
            Chord::from_event(ev(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
    }

    #[test]
    fn uppercase_letter_event_folds_to_shift_so_it_matches_a_shift_binding() {
        // Terminals deliver Cmd+Shift+P as an uppercase char; the binding is
        // written lowercase with an explicit shift. They must still match.
        let bound = Chord::parse("cmd+shift+p").unwrap();
        let live = Chord::from_event(ev(KeyCode::Char('P'), KeyModifiers::SUPER));
        assert_eq!(bound, live);
    }

    #[test]
    fn function_key_and_named_keys_parse() {
        assert_eq!(
            Chord::parse("f2"),
            Some(Chord::normalize(KeyCode::F(2), KeyModifiers::empty()))
        );
        assert_eq!(
            Chord::parse("cmd+enter").unwrap(),
            Chord::from_event(ev(KeyCode::Enter, KeyModifiers::SUPER))
        );
    }

    #[test]
    fn literal_plus_key_parses() {
        assert_eq!(
            Chord::parse("ctrl++").unwrap(),
            Chord::from_event(ev(KeyCode::Char('+'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn mod_resolves_per_platform() {
        let c = Chord::parse("mod+s").unwrap();
        let expected = if cfg!(target_os = "macos") {
            KeyModifiers::SUPER
        } else {
            KeyModifiers::CONTROL
        };
        assert_eq!(c, Chord::from_event(ev(KeyCode::Char('s'), expected)));
    }

    #[test]
    fn iterm2_forwarder_reproduces_the_hand_written_consts() {
        // These are copied from src/iterm2.rs; the generated forwarder for a
        // user binding of the same chord must match byte-for-byte, or iTerm2
        // would install a subtly different (broken) entry.
        let (k, h) = Chord::parse("cmd+f").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x66-0x100000-0x3");
        assert_eq!(h, "0x1b 0x5b 0x31 0x30 0x32 0x3b 0x39 0x75");

        let (k, h) = Chord::parse("cmd+shift+p")
            .unwrap()
            .iterm2_forwarder()
            .unwrap();
        assert_eq!(k, "0x50-0x120000-0x23");
        assert_eq!(h, "0x1b 0x5b 0x38 0x30 0x3b 0x31 0x30 0x75");

        let (k, h) = Chord::parse("cmd+.").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x2e-0x100000-0x2f");
        assert_eq!(h, "0x1b 0x5b 0x34 0x36 0x3b 0x39 0x75");

        // Matches the hand-written CMD_SHIFT_Z consts in iterm2.rs (editor redo).
        let (k, h) = Chord::parse("cmd+shift+z")
            .unwrap()
            .iterm2_forwarder()
            .unwrap();
        assert_eq!(k, "0x5a-0x120000-0x6");
        assert_eq!(h, "0x1b 0x5b 0x39 0x30 0x3b 0x31 0x30 0x75");

        let (k, _) = Chord::parse("cmd+0").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x30-0x100000-0x1d");
    }

    #[test]
    fn iterm2_forwarder_skips_chords_that_need_no_forwarding_or_cant_encode() {
        // No Super: the terminal already delivers Ctrl/Alt/function chords.
        assert_eq!(Chord::parse("ctrl+alt+g").unwrap().iterm2_forwarder(), None);
        assert_eq!(Chord::parse("f2").unwrap().iterm2_forwarder(), None);
        // Shifted punctuation is layout-dependent, so it's skipped.
        assert_eq!(
            Chord::parse("cmd+shift+/").unwrap().iterm2_forwarder(),
            None
        );
        // A key not on the ANSI table has no virtual keycode.
        assert_eq!(Chord::parse("cmd+enter").unwrap().iterm2_forwarder(), None);
    }

    #[test]
    fn junk_tokens_yield_none() {
        assert_eq!(Chord::parse("ctrl+notakey"), None);
        assert_eq!(Chord::parse(""), None);
    }

    #[test]
    fn keymap_resolves_command_ids_and_skips_bad_rows() {
        let json = r#"[
            { "key": "cmd+,", "command": "open_settings" },
            { "key": "ctrl+shift+p", "command": "quick_open" },
            { "key": "garbage++key", "command": "save_file" },
            { "key": "cmd+j", "command": "no_such_command" }
        ]"#;
        let km = Keymap::from_json(json);
        assert_eq!(
            km.command_for(ev(KeyCode::Char(','), KeyModifiers::SUPER)),
            Some(Command::OpenSettings)
        );
        assert_eq!(
            km.command_for(ev(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::QuickOpen)
        );
        // Unknown command id is dropped, not fatal.
        assert_eq!(
            km.command_for(ev(KeyCode::Char('j'), KeyModifiers::SUPER)),
            None
        );
    }

    #[test]
    fn strips_line_comments_but_keeps_slashes_in_strings() {
        let stripped =
            strip_line_comments("// lead\n[{ \"key\": \"a//b\", \"command\": \"x\" }] // trailing");
        assert!(!stripped.contains("lead"));
        assert!(!stripped.contains("trailing"));
        assert!(
            stripped.contains("a//b"),
            "// inside a quoted value must survive"
        );
    }

    #[test]
    fn empty_or_missing_json_is_empty_keymap() {
        assert!(Keymap::from_json("").is_empty());
        assert!(Keymap::from_json("not json").is_empty());
        assert!(Keymap::load(Path::new("/no/such/keybindings.json")).is_empty());
    }

    // An empty file and a broken one produce the same keymap and mean opposite
    // things: one is "I bound nothing", the other is "everything I bound is
    // gone". The test above passing made this look covered when it was not.
    #[test]
    fn a_file_that_does_not_parse_says_so_instead_of_dropping_every_binding() {
        // One trailing comma, the classic hand-edit.
        let broken = r#"[
            { "key": "cmd+1", "command": "save_file" },
        ]"#;
        let (map, warnings) = Keymap::resolve(broken);
        assert!(map.is_empty(), "a broken file binds nothing");
        assert_eq!(
            warnings.len(),
            1,
            "and must say so exactly once: {warnings:?}"
        );
        assert!(
            warnings[0].contains("does not parse"),
            "the warning must name the cause: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains("NO keybindings"),
            "and the consequence, since the file looks fine to the user: {}",
            warnings[0]
        );

        // An EMPTY file is not an error: it legitimately means no bindings,
        // and warning about it would train the reader to ignore the channel.
        let (map, warnings) = Keymap::resolve("[]");
        assert!(map.is_empty());
        assert!(
            warnings.is_empty(),
            "an empty list is not a failure: {warnings:?}"
        );
    }

    #[test]
    fn template_parses_as_a_valid_keymap() {
        // The seeded starter must itself resolve, or first-run users hit a
        // dead example. Strip the // comments the way the loader tolerates.
        let km = Keymap::from_json(TEMPLATE);
        assert_eq!(
            km.command_for(ev(
                KeyCode::Char(','),
                if cfg!(target_os = "macos") {
                    KeyModifiers::SUPER
                } else {
                    KeyModifiers::CONTROL
                }
            )),
            Some(Command::OpenSettings)
        );
    }

    // #315: a row that does not do what it says must say so. All three ways a
    // row can fail to bind were silent, and a duplicate is the worst of them:
    // the file looks like it bound both, and nothing anywhere says otherwise.
    #[test]
    fn a_duplicate_row_reports_which_binding_was_discarded() {
        let json = r#"[
            { "key": "cmd+1", "command": "save_file" },
            { "key": "cmd+1", "command": "close_editor" }
        ]"#;
        let (map, warnings) = Keymap::resolve(json);
        // VS Code semantics are preserved: the later row deliberately
        // overrides the earlier one, which is how a default gets rebound.
        assert_eq!(
            map.bindings.get(&Chord::parse("cmd+1").unwrap()).copied(),
            Some(Command::CloseEditor),
            "the later row must still win"
        );
        assert_eq!(
            warnings.len(),
            1,
            "one duplicate, one warning: {warnings:?}"
        );
        let w = &warnings[0];
        assert!(w.contains("cmd+1"), "the warning must name the chord: {w}");
        assert!(
            w.contains("close_editor") && w.contains("save_file"),
            "it must name BOTH the winner and the discarded row, or the reader \
             cannot tell which of their two lines stopped working: {w}"
        );
    }

    #[test]
    fn an_unparseable_chord_and_an_unknown_command_each_report_their_row() {
        let json = r#"[
            { "key": "cmd+shift+not-a-key", "command": "save_file" },
            { "key": "cmd+2", "command": "definitely_not_a_command" }
        ]"#;
        let (map, warnings) = Keymap::resolve(json);
        assert!(map.is_empty(), "neither row can bind");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("cmd+shift+not-a-key") && warnings[0].contains("save_file"),
            "an unparseable chord must name the chord AND the command it meant to bind: {:?}",
            warnings[0]
        );
        assert!(
            warnings[1].contains("definitely_not_a_command") && warnings[1].contains("cmd+2"),
            "an unknown command must name the command AND the chord: {:?}",
            warnings[1]
        );
    }

    // A file where every row is fine must stay silent, or the channel fills
    // with noise and the real warnings stop being findable.
    #[test]
    fn a_clean_file_produces_no_warnings() {
        let json = r#"[
            { "key": "cmd+1", "command": "save_file" },
            { "key": "cmd+2", "command": "close_editor" }
        ]"#;
        let (map, warnings) = Keymap::resolve(json);
        assert_eq!(map.chords().len(), 2);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
