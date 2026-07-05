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
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Self {
        let rows: Vec<Binding> =
            serde_json::from_str(&strip_line_comments(json)).unwrap_or_default();
        let mut bindings = HashMap::new();
        for row in rows {
            if let (Some(chord), Some(cmd)) =
                (Chord::parse(&row.key), Command::from_id(&row.command))
            {
                bindings.insert(chord, cmd);
            }
        }
        Self { bindings }
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
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
}
